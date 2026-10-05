//! Rendering a transcript to a file format.

use std::time::Duration;

use crate::domain::Transcript;

/// Output formats a transcript can be written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Plain text, one segment per line.
    Text,
    /// SubRip subtitles, with timestamps.
    Srt,
}

impl Format {
    /// Guess a format from a file extension.
    ///
    /// Returns `None` for anything unrecognised rather than silently picking
    /// one — writing SRT into a file the user named `.txt` would be worse than
    /// asking.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "txt" | "text" => Some(Format::Text),
            "srt" => Some(Format::Srt),
            _ => None,
        }
    }

    /// The canonical extension for this format.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Text => "txt",
            Format::Srt => "srt",
        }
    }

    pub fn render(self, transcript: &Transcript) -> String {
        match self {
            Format::Text => to_text(transcript),
            Format::Srt => to_srt(transcript),
        }
    }
}

/// Render as plain text, one segment per line.
pub fn to_text(transcript: &Transcript) -> String {
    let mut out = transcript.to_text();
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Render as SubRip subtitles.
///
/// Cues are numbered from 1 and separated by blank lines, with timestamps as
/// `HH:MM:SS,mmm` — commas, not dots, or most players reject the file.
///
/// Empty segments are skipped. A cue with no text is a rendering bug, not a
/// subtitle, and a player will show it as a stray blank flash.
pub fn to_srt(transcript: &Transcript) -> String {
    let mut out = String::new();
    let mut index = 0usize;

    for segment in &transcript.segments {
        if segment.text.trim().is_empty() {
            continue;
        }
        index += 1;

        out.push_str(&index.to_string());
        out.push('\n');
        out.push_str(&format_timestamp(segment.start));
        out.push_str(" --> ");
        out.push_str(&format_timestamp(segment.end));
        out.push('\n');
        out.push_str(segment.text.trim());
        out.push_str("\n\n");
    }

    out
}

/// `HH:MM:SS,mmm`, the SubRip timestamp format.
fn format_timestamp(duration: Duration) -> String {
    let total_ms = duration.as_millis();
    let millis = total_ms % 1_000;
    let total_secs = total_ms / 1_000;
    let seconds = total_secs % 60;
    let minutes = (total_secs / 60) % 60;
    let hours = total_secs / 3_600;

    format!("{hours:02}:{minutes:02}:{seconds:02},{millis:03}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Segment, SegmentId};

    fn segment(id: u64, start_ms: u64, end_ms: u64, text: &str) -> Segment {
        Segment {
            id: SegmentId(id),
            start: Duration::from_millis(start_ms),
            end: Duration::from_millis(end_ms),
            text: text.to_string(),
        }
    }

    fn transcript(segments: Vec<Segment>) -> Transcript {
        Transcript {
            segments,
            language: None,
        }
    }

    #[test]
    fn timestamps_use_the_subrip_format() {
        assert_eq!(format_timestamp(Duration::ZERO), "00:00:00,000");
        assert_eq!(format_timestamp(Duration::from_millis(1)), "00:00:00,001");
        assert_eq!(
            format_timestamp(Duration::from_millis(1_500)),
            "00:00:01,500"
        );
        assert_eq!(format_timestamp(Duration::from_secs(3_661)), "01:01:01,000");
        // Hours are not capped; a long recording must not wrap.
        assert_eq!(
            format_timestamp(Duration::from_secs(36_000)),
            "10:00:00,000"
        );
    }

    #[test]
    fn srt_is_numbered_and_blank_line_separated() {
        let t = transcript(vec![
            segment(0, 0, 1_500, "第一句"),
            segment(1, 1_500, 3_000, "第二句"),
        ]);

        let srt = to_srt(&t);
        assert_eq!(
            srt,
            "1\n00:00:00,000 --> 00:00:01,500\n第一句\n\n\
             2\n00:00:01,500 --> 00:00:03,000\n第二句\n\n"
        );
    }

    #[test]
    fn empty_segments_are_skipped_rather_than_numbered() {
        let t = transcript(vec![
            segment(0, 0, 1_000, "有内容"),
            segment(1, 1_000, 2_000, "   "),
            segment(2, 2_000, 3_000, "又有内容"),
        ]);

        let srt = to_srt(&t);
        // Two cues, numbered 1 and 2 - the blank one must not take a number or
        // leave a gap.
        assert!(srt.starts_with("1\n"));
        assert!(srt.contains("\n2\n"));
        assert!(!srt.contains("\n3\n"));
        assert_eq!(srt.matches(" --> ").count(), 2);
    }

    #[test]
    fn an_empty_transcript_renders_as_nothing() {
        assert_eq!(to_srt(&transcript(vec![])), "");
        assert_eq!(to_text(&transcript(vec![])), "");
    }

    #[test]
    fn text_output_is_one_segment_per_line() {
        let t = transcript(vec![
            segment(0, 0, 1_000, "第一句"),
            segment(1, 1_000, 2_000, "第二句"),
        ]);
        assert_eq!(to_text(&t), "第一句\n第二句\n");
    }

    #[test]
    fn format_is_guessed_from_the_extension() {
        assert_eq!(Format::from_extension("srt"), Some(Format::Srt));
        assert_eq!(Format::from_extension("SRT"), Some(Format::Srt));
        assert_eq!(Format::from_extension("txt"), Some(Format::Text));
        assert_eq!(Format::from_extension("docx"), None);
    }
}
