//! Running a transcription.
//!
//! The chain itself: decode, segment, recognise, publish. Shared by the
//! command line, the application and the benchmark.
//!
//! It lives in its own crate rather than in `verse-core` because it is
//! orchestration — it names concrete engines and decoders, which is exactly
//! what the core crate is built not to know. And it is shared rather than
//! copied because a benchmark measuring a second implementation of the
//! pipeline would be measuring the wrong thing.

mod coverage;

use std::path::{Path, PathBuf};

use verse_asr::register_builtin_engines;
use verse_audio::{FfmpegDecoder, FixedBlocks, SileroVad};
use verse_core::{
    AsrEngine, AudioChunk, AudioFormat, AudioSource, CancelToken, Error, Event, EventBus,
    HardwareProfile, JobId, JobKind, Registry, SegmentId, Segmenter, Transcript,
};

pub use coverage::{
    Coverage, CoverageMeter, GuardSettings, DEFAULT_FLOOR, DEFAULT_MIN_ENERGETIC_SECONDS,
};

/// What a job needs to run.
#[derive(Debug, Clone)]
pub struct Request {
    pub input: PathBuf,
    pub models_dir: PathBuf,
    pub engine: String,
    pub vad_model: PathBuf,
    /// Whether the engine may rewrite written forms. See
    /// [`verse_core::EngineConfig::inverse_text_normalization`].
    pub inverse_text_normalization: bool,
    /// How the voice detector decides where speech starts and stops.
    pub vad: verse_audio::VadSettings,
    /// Longest output a generative engine may produce. See
    /// [`verse_core::EngineConfig::max_output_tokens`].
    pub max_output_tokens: Option<i32>,
    /// When to disbelieve the segmenter and recognise the file whole.
    pub guard: GuardSettings,
}

impl Request {
    /// The VAD model that goes with a models root.
    pub fn vad_for(models_dir: &Path) -> PathBuf {
        models_dir.join("silero-vad").join("silero_vad.onnx")
    }
}

/// What one run produced.
///
/// The transcript is what the caller asked for; the coverage is what the
/// segmenter did on the way, kept because a short transcript and a truncated
/// one look identical from the outside and this is the only thing that tells
/// them apart.
#[derive(Debug, Clone)]
pub struct Transcription {
    pub transcript: Transcript,
    pub coverage: Coverage,
    /// Whether the guard discarded the first pass and recognised the file
    /// whole. Recorded because a recovered file is slower than a normal one by
    /// design, and a caller measuring cost needs to know which it got.
    pub recovered: bool,
}

/// A loaded recogniser, ready to transcribe files one after another.
///
/// **The model is loaded once.** That is the entire reason this is a struct
/// rather than a function: loading a 228 MB model takes longer than
/// transcribing a short utterance, so a caller scoring thousands of them
/// cannot afford to do it per file. The command line and the window each
/// transcribe one file and pay it once; the benchmark scores thousands and
/// would otherwise spend all its time on disk.
///
/// The VAD model is *not* kept: it is 2.3 MB, loading it is immediate, and a
/// fresh one per file removes any question of state carried between them.
pub struct Transcriber {
    request: Request,
    engine: Box<dyn AsrEngine>,
}

impl Transcriber {
    /// Load the engine named by `request`.
    pub fn load(request: Request) -> Result<Self, Error> {
        let hardware = HardwareProfile::probe();

        let mut registry = Registry::new();
        register_builtin_engines(&mut registry);
        let engine = registry.create_engine(
            &request.engine,
            &verse_core::EngineConfig {
                model_dir: request.models_dir.join(&request.engine),
                threads: hardware.engine_threads(),
                inverse_text_normalization: request.inverse_text_normalization,
                max_output_tokens: request.max_output_tokens,
            },
        )?;

        Ok(Self { request, engine })
    }

    /// Transcribe the file named by the request, publishing progress as it
    /// goes.
    ///
    /// Every outcome is announced on the bus before this returns, including
    /// failure, so the caller does not have to publish anything itself. The
    /// returned transcript is for the caller's own use; the interface has
    /// already been told.
    pub fn transcribe(
        &mut self,
        job: JobId,
        bus: &EventBus,
        cancel: &CancelToken,
    ) -> Result<Transcription, Error> {
        bus.publish(Event::JobStarted {
            id: job,
            kind: JobKind::FileTranscribe,
        });

        match self.run(job, bus, cancel) {
            Ok(transcription) => {
                bus.publish(Event::TranscriptFinal {
                    job,
                    transcript: transcription.transcript.clone(),
                });
                bus.publish(Event::JobFinished { id: job });
                Ok(transcription)
            }
            Err(error) => {
                // A cancelled job is not a failure and gets its own event, so
                // the interface can tell "stopped" apart from "broke".
                if error.kind() == verse_core::ErrorKind::Cancelled {
                    bus.publish(Event::JobCancelled { id: job });
                } else {
                    bus.publish(Event::JobFailed {
                        id: job,
                        error: (&error).into(),
                    });
                }
                Err(error)
            }
        }
    }

    /// Where this transcriber's files come from.
    pub fn request(&self) -> &Request {
        &self.request
    }

    /// Point it at a different file, keeping the loaded model.
    pub fn set_input(&mut self, input: PathBuf) {
        self.request.input = input;
    }

    fn run(
        &mut self,
        job: JobId,
        bus: &EventBus,
        cancel: &CancelToken,
    ) -> Result<Transcription, Error> {
        let mut vad = SileroVad::load_with(
            &self.request.vad_model,
            AudioFormat::TARGET,
            self.request.vad,
        )?;
        let (transcript, coverage) = self.sweep(&mut vad, job, bus, cancel)?;

        if !self.request.guard.should_recover(&coverage) {
            return Ok(Transcription {
                transcript,
                coverage,
                recovered: false,
            });
        }

        // The detector has reported almost no speech in a file that plainly
        // contains some, so its spans are not the recording — they are what it
        // could hear of it. Recognise the file whole instead. Everything
        // published so far is withdrawn first, or a listener would be left
        // showing the fragment this is replacing.
        bus.publish(Event::TranscriptDiscarded { job });

        let mut whole = FixedBlocks::new(AudioFormat::TARGET);
        let (transcript, _) = self.sweep(&mut whole, job, bus, cancel)?;

        // The coverage reported is the first pass's: it is the reading that
        // explains why this file was treated differently, and the second
        // pass's is one by construction.
        Ok(Transcription {
            transcript,
            coverage,
            recovered: true,
        })
    }

    /// One pass over the file with a given segmenter.
    fn sweep(
        &mut self,
        segmenter: &mut dyn Segmenter,
        job: JobId,
        bus: &EventBus,
        cancel: &CancelToken,
    ) -> Result<(Transcript, Coverage), Error> {
        let request = &self.request;
        let engine = self.engine.as_mut();

        let mut source = FfmpegDecoder::open(&request.input, AudioFormat::TARGET)?;
        let mut meter = CoverageMeter::new(AudioFormat::TARGET.sample_rate);
        let mut transcript = Transcript::default();

        while let Some(chunk) = source.next_chunk()? {
            cancel.check()?;

            meter.observe(&chunk.samples);
            segmenter.accept(&chunk)?;
            for span in segmenter.take() {
                meter.kept(span.start, span.samples.len());
                recognize(span, &mut *engine, &mut transcript, job, bus)?;
            }

            bus.publish(Event::JobProgress {
                id: job,
                position: chunk.end(),
                // The decoder does not report a total length, and adding a probe
                // pass to get one would cost a second read of the file. The
                // interface shows elapsed time and recognised segments instead.
                fraction: 0.0,
            });
        }

        for span in segmenter.finish() {
            meter.kept(span.start, span.samples.len());
            recognize(span, &mut *engine, &mut transcript, job, bus)?;
        }

        Ok((transcript, meter.finish()))
    }
}

/// Recognise one span, publishing each segment as it lands.
///
/// The engine is reused across spans and reset between each, which is what
/// bounds memory: a span is recognised and dropped before the next is
/// buffered, so a two-hour recording is never resident all at once.
fn recognize(
    span: AudioChunk,
    engine: &mut dyn AsrEngine,
    out: &mut Transcript,
    job: JobId,
    bus: &EventBus,
) -> Result<(), Error> {
    engine.reset();
    engine.accept(&span)?;
    let piece = engine.finalize()?;

    for segment in piece.segments {
        // Each span's engine reports segment id 0; renumber so ids stay unique
        // across the whole transcript.
        let segment = verse_core::Segment {
            id: SegmentId(out.segments.len() as u64),
            ..segment
        };


        bus.publish(Event::TranscriptSegment {
            job,
            segment: segment.clone(),
        });
        out.segments.push(segment);
    }

    Ok(())
}
