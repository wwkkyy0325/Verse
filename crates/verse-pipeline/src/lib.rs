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

mod cache;
mod coverage;
mod keep;

#[cfg(test)]
pub(crate) mod testing;

use std::path::{Path, PathBuf};

use verse_asr::register_builtin_engines;
use verse_audio::{FfmpegDecoder, FixedBlocks, SileroVad};
use verse_core::{
    AsrEngine, AudioChunk, AudioFormat, AudioSource, CancelToken, Error, Event, EventBus,
    HardwareProfile, JobId, JobKind, Registry, SegmentId, Segmenter, Transcript,
};

pub use cache::{from_entry, settings_digest, to_entry, CachePolicy};
pub use keep::{ModelKeeper, ModelStatus, DEFAULT_IDLE};
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
    /// Domain vocabulary for an engine that can use one. See
    /// [`verse_core::EngineConfig::hotwords`].
    pub hotwords: Option<String>,
    /// CPU threads for the engine, or `None` for the hardware probe's own
    /// figure.
    ///
    /// Worth setting only when several transcriptions run at once. That figure
    /// is already cores − 1, so four workers taking it each would ask the
    /// machine for four times what it has.
    pub threads: Option<usize>,
    /// Whether to look for this transcription already done, and where.
    pub cache: CachePolicy,
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
    /// Whether this came out of the cache rather than out of the engine.
    ///
    /// Travels with the result because it is the difference between "this took
    /// four minutes" and "this took no time at all", and a caller comparing
    /// runs — or a person wondering why the second one was instant — has no
    /// other way to tell.
    pub cached: bool,
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
    /// The digest of everything about this run except the audio, computed once
    /// at load.
    ///
    /// Costing it per file would be a mistake worth avoiding: it stats every
    /// file of a 228 MB model and asks ffmpeg its version, and a batch of a
    /// hundred files would pay that a hundred times to learn the same answer.
    settings: String,
}

/// Whether the named engine can use a domain vocabulary.
///
/// Answerable **without loading a model**, which is the point: a warning about
/// a lexicon that will be ignored is worth having before a 228 MB wait, not
/// after it. Read from the descriptor rather than compared against a list of
/// names, so the pipeline still does not know which engines exist.
pub fn engine_accepts_hotwords(engine: &str) -> bool {
    let mut registry = Registry::new();
    register_builtin_engines(&mut registry);
    registry
        .engine(engine)
        .is_some_and(|descriptor| descriptor.supports_hotwords)
}

/// How many threads this request will actually run with.
///
/// A caller leaving `threads` unset gets cores − 1. Exposed because two places
/// now need the answer — loading an engine, and deciding whether an engine
/// already loaded is still the right one — and two rules for it would eventually
/// disagree.
pub fn effective_threads(request: &Request) -> usize {
    request
        .threads
        .unwrap_or_else(|| HardwareProfile::probe().engine_threads())
}

impl Transcriber {
    /// Load the engine named by `request`.
    pub fn load(request: Request) -> Result<Self, Error> {
        let threads = effective_threads(&request);

        let mut registry = Registry::new();
        register_builtin_engines(&mut registry);

        let engine = registry.create_engine(
            &request.engine,
            &verse_core::EngineConfig {
                model_dir: request.models_dir.join(&request.engine),
                threads,
                inverse_text_normalization: request.inverse_text_normalization,
                max_output_tokens: request.max_output_tokens,
                hotwords: request.hotwords.clone(),
            },
        )?;

        // The resolved thread count goes into the key, not `request.threads`.
        // A caller leaving it unset gets cores − 1, so two machines run the
        // same request with different thread counts — and ONNX reduction order
        // is not invariant under that. Recording the request rather than the
        // answer would let a cache carried between machines serve a result
        // computed differently.
        let settings = settings_digest(&request, threads, verse_audio::ffmpeg_version().as_deref());

        Ok(Self {
            request,
            engine,
            settings,
        })
    }

    /// The identity of the configuration this engine was built from.
    ///
    /// Exactly what the cache keys on, and for the same reason: two runs that
    /// produce this same string would build the same engine and read the same
    /// run-scoped settings. `crate::keep` compares it to decide whether a
    /// loaded model can be reused.
    pub fn settings(&self) -> &str {
        &self.settings
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

        // The cache is consulted here rather than in each front-end, and that
        // placement is the whole reason a hit behaves. Every caller — the
        // command line, the window, the benchmark — reaches the engine through
        // this one method, so the event sequence below is written once instead
        // of three times and cannot drift between them.
        if let Some((cache, key)) = self.cache_key() {
            if let Some(entry) = cache.get(&key) {
                let transcription = from_entry(&entry);
                announce(bus, job, &transcription);
                return Ok(transcription);
            }
        }

        match self.run(job, bus, cancel) {
            Ok(transcription) => {
                bus.publish(Event::TranscriptFinal {
                    job,
                    transcript: transcription.transcript.clone(),
                });
                bus.publish(Event::JobFinished { id: job });

                // After the events, so a failure to store cannot be mistaken
                // for a failure to transcribe. Storing is best-effort by
                // design: the result is already in hand and the cache is a
                // convenience.
                self.store(&transcription);

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

    /// The cache to look in and the key to look up, when there is one.
    ///
    /// `None` for every reason not to look: the cache is off, or the file
    /// cannot be read to hash it. None of those is an error — the run simply
    /// happens.
    fn cache_key(&self) -> Option<(verse_store::Cache, String)> {
        let (cache, verify) = self.request.cache.open()?;
        let content = verse_store::content_digest(&self.request.input, verify).ok()?;
        Some((cache.clone(), verse_store::key(&self.settings, &content)))
    }

    /// Keep a finished run, if there is anywhere to keep it.
    fn store(&self, transcription: &Transcription) {
        let Some((cache, key)) = self.cache_key() else {
            return;
        };
        // Deliberately not reported. A cache that cannot be written makes the
        // next run slower and nothing else, and a warning about it on every
        // run of a read-only volume would be noise in place of information.
        let _ = cache.put(&key, &to_entry(transcription));
    }

    /// Where this transcriber's files come from.
    pub fn request(&self) -> &Request {
        &self.request
    }

    /// Point it at a different file, keeping the loaded model.
    pub fn set_input(&mut self, input: PathBuf) {
        self.request.input = input;
    }

    /// Change where this transcriber looks for already-done work.
    ///
    /// Per-run, not per-model: which results may be reused is a question about
    /// the caller, not about the weights. `crate::keep` sets it for every job so
    /// that a keeper reusing one model across callers cannot serve a job's
    /// results under a different caller's policy.
    ///
    /// Safe to change after loading because the settings digest does not cover
    /// it — see `cache::settings_digest` — so `settings` stays true.
    pub fn set_cache_policy(&mut self, cache: CachePolicy) {
        self.request.cache = cache;
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
        let (transcript, coverage) = self.sweep(&mut vad, job, bus, cancel, "vad")?;

        if !self.request.guard.should_recover(&coverage) {
            return Ok(Transcription {
                transcript,
                coverage,
                recovered: false,
                cached: false,
            });
        }

        // The detector has reported almost no speech in a file that plainly
        // contains some, so its spans are not the recording — they are what it
        // could hear of it. Recognise the file whole instead. Everything
        // published so far is withdrawn first, or a listener would be left
        // showing the fragment this is replacing.
        bus.publish(Event::TranscriptDiscarded { job });

        let mut whole = FixedBlocks::new(AudioFormat::TARGET);
        let (transcript, _) = self.sweep(&mut whole, job, bus, cancel, "whole")?;

        // The coverage reported is the first pass's: it is the reading that
        // explains why this file was treated differently, and the second
        // pass's is one by construction.
        Ok(Transcription {
            transcript,
            coverage,
            recovered: true,
            cached: false,
        })
    }

    /// One pass over the file with a given segmenter.
    ///
    /// `pass` names which segmenter this is, so the two passes a guarded run
    /// can make never share a checkpoint: the detector's spans and the
    /// whole-file fallback's have nothing to do with each other, and reusing
    /// across them would splice two segmentations into one transcript.
    fn sweep(
        &mut self,
        segmenter: &mut dyn Segmenter,
        job: JobId,
        bus: &EventBus,
        cancel: &CancelToken,
        pass: &str,
    ) -> Result<(Transcript, Coverage), Error> {
        let mut progress = self.open_progress(pass);

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
                recognize(
                    span,
                    &mut *engine,
                    &mut transcript,
                    job,
                    bus,
                    progress.as_mut(),
                )?;
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
            recognize(
                span,
                &mut *engine,
                &mut transcript,
                job,
                bus,
                progress.as_mut(),
            )?;
        }

        // Reached only by reading the whole file. A run that returns early —
        // cancelled, or failed — leaves its log behind, which is the point of
        // having one.
        if let Some(progress) = progress.take() {
            let _ = progress.forget();
        }

        Ok((transcript, meter.finish()))
    }

    /// A checkpoint log for one pass of this run, when there is anywhere to
    /// keep one.
    ///
    /// Tied to the cache switch on purpose. `--no-cache` and `VERSE_NO_CACHE`
    /// mean "do not reuse anything"; reading a resume log would be reusing.
    fn open_progress(&self, pass: &str) -> Option<verse_store::Progress> {
        let (cache, _) = self.request.cache.open()?;
        let data = cache.data_dir()?;
        // The same key the result cache uses, so a checkpoint can never be read
        // by a run that would not have been allowed to read the result.
        let (_, key) = self.cache_key()?;
        Some(verse_store::Progress::open(&data, &key, pass))
    }
}

/// Tell the bus that a result is finished, exactly as a run would have.
///
/// **Every segment, not just the final transcript.** The window assembles the
/// list it shows from `TranscriptSegment`; `TranscriptFinal` only finalises.
/// A cached result announced with the final transcript alone would therefore
/// render as "没有识别到内容" while reporting success — an empty transcript
/// that looks precisely like a finished one, which is the failure this whole
/// project keeps having to design against.
///
/// The order matches `run`'s for the same reason: a listener that has been
/// told the sequence either way must not be able to tell which happened.
fn announce(bus: &EventBus, job: JobId, transcription: &Transcription) {
    for segment in &transcription.transcript.segments {
        bus.publish(Event::TranscriptSegment {
            job,
            segment: segment.clone(),
        });
    }
    bus.publish(Event::TranscriptFinal {
        job,
        transcript: transcription.transcript.clone(),
    });
    bus.publish(Event::JobFinished { id: job });
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
    progress: Option<&mut verse_store::Progress>,
) -> Result<(), Error> {
    let key = progress
        .as_ref()
        .map(|_| verse_store::span_key(span.start, &span.samples));

    // Already done — by this attempt, or by one that was interrupted before it
    // finished. The text is replayed through the same events a fresh
    // recognition would publish, so a resumed run cannot be told apart from an
    // uninterrupted one by anything downstream.
    if let (Some(progress), Some(key)) = (progress.as_ref(), key.as_ref()) {
        if let Some(stored) = progress.remembered(key) {
            for remembered in stored {
                let segment = verse_core::Segment {
                    id: SegmentId(out.segments.len() as u64),
                    start: std::time::Duration::from(remembered.start),
                    end: std::time::Duration::from(remembered.end),
                    text: remembered.text.clone(),
                };
                bus.publish(Event::TranscriptSegment {
                    job,
                    segment: segment.clone(),
                });
                out.segments.push(segment);
            }
            return Ok(());
        }
    }

    engine.reset();
    engine.accept(&span)?;
    let piece = engine.finalize()?;

    let mut produced: Vec<verse_store::SegmentDto> = Vec::new();
    for segment in piece.segments {
        // Each span's engine reports segment id 0; renumber so ids stay unique
        // across the whole transcript.
        let segment = verse_core::Segment {
            id: SegmentId(out.segments.len() as u64),
            ..segment
        };

        produced.push(verse_store::SegmentDto {
            id: segment.id.0,
            start: segment.start.into(),
            end: segment.end.into(),
            text: segment.text.clone(),
        });

        bus.publish(Event::TranscriptSegment {
            job,
            segment: segment.clone(),
        });
        out.segments.push(segment);
    }

    // Best effort, and deliberately last. A checkpoint that cannot be written
    // makes a future interruption more expensive and nothing else — the
    // transcript in hand is already correct and already published.
    if let (Some(progress), Some(key)) = (progress, key) {
        let _ = progress.record(&key, &produced);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use crate::testing::{a_chunk, Fixed};
    use verse_core::{Segment, Subscription};

    fn a_transcription(texts: &[&str]) -> Transcription {
        Transcription {
            transcript: Transcript {
                segments: texts
                    .iter()
                    .enumerate()
                    .map(|(index, text)| Segment {
                        id: SegmentId(index as u64),
                        start: Duration::from_millis(index as u64 * 1_000),
                        end: Duration::from_millis(index as u64 * 1_000 + 900),
                        text: (*text).to_string(),
                    })
                    .collect(),
                language: None,
            },
            coverage: Coverage {
                decoded_seconds: 2.0,
                voiced_seconds: 2.0,
                energetic_seconds: 2.0,
            },
            recovered: false,
            cached: true,
        }
    }

    /// What a listener sees, as a compact list of names.
    fn announced(subscription: &Subscription) -> Vec<String> {
        subscription
            .drain()
            .iter()
            .map(|event| match &**event {
                Event::TranscriptSegment { segment, .. } => format!("segment:{}", segment.text),
                Event::TranscriptFinal { .. } => "final".to_string(),
                Event::JobFinished { .. } => "finished".to_string(),
                other => format!("other:{other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_cached_result_is_announced_segment_by_segment() {
        // The assertion that matters. The window builds what it shows from
        // `TranscriptSegment`, so a replay that skipped them would put an
        // empty transcript on screen while reporting success — a result that
        // looks finished and is not.
        let bus = EventBus::new();
        let subscription = bus.subscribe_all();

        announce(&bus, JobId(1), &a_transcription(&["开放时间", "上午九点"]));
        let seen = announced(&subscription);

        assert_eq!(
            seen,
            ["segment:开放时间", "segment:上午九点", "final", "finished"]
        );
    }

    #[test]
    fn the_replay_carries_the_text_not_just_the_count() {
        // Counts would be satisfied by publishing placeholder segments, which
        // would be a worse failure than publishing none: it would look right.
        let bus = EventBus::new();
        let subscription = bus.subscribe_all();

        announce(&bus, JobId(1), &a_transcription(&["会议记录"]));

        let text: Vec<String> = subscription
            .drain()
            .iter()
            .filter_map(|event| match &**event {
                Event::TranscriptFinal { transcript, .. } => {
                    Some(transcript.segments.iter().map(|s| s.text.clone()).collect())
                }
                _ => None,
            })
            .next()
            .expect("a final transcript");

        assert_eq!(text, ["会议记录"]);
    }

    #[test]
    fn an_empty_result_still_says_it_finished() {
        // A silent file is a legitimate outcome, not a failure, and a listener
        // waiting for the end must still be told.
        let bus = EventBus::new();
        let subscription = bus.subscribe_all();

        let mut transcription = a_transcription(&[]);
        transcription.transcript.segments.clear();
        announce(&bus, JobId(1), &transcription);

        assert_eq!(announced(&subscription), ["final", "finished"]);
    }

    #[test]
    fn the_events_name_the_job_they_belong_to() {
        // Two jobs can be in flight — the window cancels one and starts
        // another — so an event without its job is an event a listener cannot
        // place.
        let bus = EventBus::new();
        let subscription = bus.subscribe_all();

        announce(&bus, JobId(7), &a_transcription(&["x"]));

        for event in subscription.drain() {
            let id = match &*event {
                Event::TranscriptSegment { job, .. }
                | Event::TranscriptFinal { job, .. }
                | Event::JobFinished { id: job } => *job,
                other => panic!("unexpected event: {other:?}"),
            };
            assert_eq!(id, JobId(7));
        }
    }

    #[test]
    fn recognising_a_span_records_it_for_a_later_attempt() {
        // This is the test that was missing. The end-to-end run found the bug
        // first: `recognize` collected the segments it had produced and then
        // never stored them, so every checkpoint stayed empty and resume did
        // nothing at all — while the code read as though it worked and clippy
        // had nothing to say, because a Vec that is only pushed to counts as
        // used.
        let dir = std::env::temp_dir().join("verse-pipeline-test-record");
        let _ = std::fs::remove_dir_all(&dir);

        let mut progress = verse_store::Progress::open(&dir, "run", "vad");
        let mut engine = Fixed(Some(Transcript {
            segments: vec![Segment {
                id: SegmentId(0),
                start: Duration::from_millis(0),
                end: Duration::from_millis(900),
                text: "开放时间".to_string(),
            }],
            language: None,
        }));
        let mut out = Transcript::default();
        let bus = EventBus::new();

        let span = a_chunk(1_000, 16);
        let key = verse_store::span_key(span.start, &span.samples);
        recognize(span, &mut engine, &mut out, JobId(1), &bus, Some(&mut progress))
            .expect("recognise");

        assert_eq!(progress.len(), 1, "the span must have been recorded");
        let stored = progress.remembered(&key).expect("recorded under its own key");
        assert_eq!(stored[0].text, "开放时间");

        // And it survives being reopened, which is the whole point: the log
        // exists for the process that does not get to finish.
        let again = verse_store::Progress::open(&dir, "run", "vad");
        assert_eq!(again.remembered(&key).map(<[verse_store::SegmentDto]>::len), Some(1));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_attempt_reuses_the_span_without_asking_the_engine() {
        // The engine is given nothing to say, so if the text still comes out
        // it can only have come from the checkpoint.
        let dir = std::env::temp_dir().join("verse-pipeline-test-reuse");
        let _ = std::fs::remove_dir_all(&dir);

        let span = a_chunk(2_000, 16);
        let key = verse_store::span_key(span.start, &span.samples);

        let mut first = verse_store::Progress::open(&dir, "run", "vad");
        first
            .record(
                &key,
                &[verse_store::SegmentDto {
                    id: 0,
                    start: Duration::from_millis(2_000).into(),
                    end: Duration::from_millis(2_900).into(),
                    text: "从检查点读出来的".to_string(),
                }],
            )
            .expect("record");

        let mut progress = verse_store::Progress::open(&dir, "run", "vad");
        let mut engine = Fixed(None);
        let mut out = Transcript::default();
        let bus = EventBus::new();
        let subscription = bus.subscribe_all();

        recognize(
            a_chunk(2_000, 16),
            &mut engine,
            &mut out,
            JobId(1),
            &bus,
            Some(&mut progress),
        )
        .expect("recognise");

        assert_eq!(out.segments.len(), 1);
        assert_eq!(out.segments[0].text, "从检查点读出来的");
        // Only the segment: `recognize` announces segments, and the final
        // transcript is published by its caller once every span is done.
        assert_eq!(
            announced(&subscription),
            ["segment:从检查点读出来的"],
            "a reused span is announced exactly as a fresh one is"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_span_the_engine_has_never_seen_is_not_reused() {
        // Different audio is a different span, whatever else matches.
        let dir = std::env::temp_dir().join("verse-pipeline-test-noreuse");
        let _ = std::fs::remove_dir_all(&dir);

        let recorded = a_chunk(3_000, 16);
        let key = verse_store::span_key(recorded.start, &recorded.samples);
        let mut first = verse_store::Progress::open(&dir, "run", "vad");
        first
            .record(
                &key,
                &[verse_store::SegmentDto {
                    id: 0,
                    start: Duration::from_millis(3_000).into(),
                    end: Duration::from_millis(3_900).into(),
                    text: "the old audio".to_string(),
                }],
            )
            .expect("record");

        let mut progress = verse_store::Progress::open(&dir, "run", "vad");
        let mut engine = Fixed(Some(Transcript {
            segments: vec![Segment {
                id: SegmentId(0),
                start: Duration::from_millis(3_000),
                end: Duration::from_millis(3_900),
                text: "the new audio".to_string(),
            }],
            language: None,
        }));
        let mut out = Transcript::default();

        // Same position and length, different samples.
        let mut different = a_chunk(3_000, 16);
        different.samples[0] = 0.25;

        recognize(
            different,
            &mut engine,
            &mut out,
            JobId(1),
            &EventBus::new(),
            Some(&mut progress),
        )
        .expect("recognise");

        assert_eq!(out.segments[0].text, "the new audio");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
