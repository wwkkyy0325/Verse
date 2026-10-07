use std::time::Duration;

/// Identifier for a transcription job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(pub u64);

/// Identifier for a single transcript segment within a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SegmentId(pub u64);

/// Identifier for a model. Models are known at compile time, so this is a
/// static string rather than an owned one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModelId(pub &'static str);

/// PCM layout of an audio stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

impl AudioFormat {
    /// The format the recognition engines expect.
    pub const TARGET: Self = Self {
        sample_rate: 16_000,
        channels: 1,
    };
}

/// A block of mono f32 PCM samples with its offset from the start of the
/// stream. Timestamps are carried here so segments can be reconstructed later.
#[derive(Debug, Clone)]
pub struct AudioChunk {
    pub samples: Vec<f32>,
    pub format: AudioFormat,
    pub start: Duration,
}

impl AudioChunk {
    pub fn duration(&self) -> Duration {
        if self.format.sample_rate == 0 {
            return Duration::ZERO;
        }
        let frames = self.samples.len() as u64 / self.format.channels.max(1) as u64;
        Duration::from_secs_f64(frames as f64 / self.format.sample_rate as f64)
    }

    pub fn end(&self) -> Duration {
        self.start + self.duration()
    }
}

/// A finalized span of recognized speech.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub id: SegmentId,
    pub start: Duration,
    pub end: Duration,
    pub text: String,
}

/// Incremental recognition output from a streaming engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptDelta {
    pub segment: SegmentId,
    pub text: String,
    /// True once the engine will not revise this text again.
    pub stable: bool,
}

/// The complete result of a job.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transcript {
    pub segments: Vec<Segment>,
    pub language: Option<String>,
}

impl Transcript {
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Concatenate all segment text, separated by newlines.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for seg in &self.segments {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&seg.text);
        }
        out
    }
}

/// Lifecycle of a model on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelState {
    /// Not present locally.
    Absent,
    Downloading,
    Ready,
    Failed(String),
}

/// Containers ffmpeg can pull an audio track out of.
///
/// Here rather than beside either caller, because two callers need it and they
/// must not disagree: the command line expands a directory into these, and the
/// window refuses what is not one before it starts. A second copy is a second
/// thing that can drift — as the service's route list already is.
///
/// **A proxy, not the truth.** ffmpeg is the only thing that actually knows,
/// which is why the command line still attempts a file *named* on it whatever
/// it is called. The window is stricter on purpose: somebody who drags the
/// wrong thing should be told before waiting, not after. So a file with an
/// unusual but decodable extension is refused by the window and accepted by
/// the command line, and that is the intended difference rather than a bug.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "wav", "mp3", "mp2", "m4a", "m4b", "aac", "flac", "ogg", "oga", "opus", "wma", "amr", "aiff",
    "aif", "caf", "ape", "wv", "mp4", "mkv", "mov", "webm", "avi", "ts",
];

/// Whether a path looks like something this program can decode.
///
/// Case-insensitive: `.MP3` is an mp3, and on Windows it usually is exactly
/// that — a file whose name was capitalised by something that did not care.
pub fn looks_like_audio(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|extension| AUDIO_EXTENSIONS.contains(&extension.as_str()))
}
