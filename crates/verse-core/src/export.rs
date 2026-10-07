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

/// Read SubRip subtitles back into a transcript.
///
/// The inverse of [`to_srt`], in the same module so the two cannot disagree
/// about the format. It exists because the `.srt` on disk **is** the result:
/// showing a finished transcript again means reading that file, not keeping a
/// second copy of it in a database that can drift from the one a person can
/// open in a player.
///
/// Forgiving about what does not matter, strict about what does:
///
/// - **The cue number is read and discarded.** It is a position, not
///   information. A file somebody edited may have them duplicated or out of
///   order, and a player accepts that — refusing it here would refuse a file
///   that works.
/// - **A comma or a dot** between seconds and milliseconds. SubRip says comma;
///   several tools write a dot; players take both.
/// - **Cue settings after the end timestamp** are ignored, as a player ignores
///   them.
/// - **A block with no `-->` is skipped**, not treated as text — a stray header
///   or a title card must not become a subtitle line.
///
/// `None` when the text has content but no cue in it, which is a file that is
/// not subtitles. Blank input is an empty transcript rather than an error: that
/// is what [`to_srt`] writes for a transcript with nothing in it.
pub fn parse_srt(text: &str) -> Option<Transcript> {
    if text.trim().is_empty() {
        return Some(Transcript::default());
    }

    let mut blocks: Vec<Vec<&str>> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        blocks.push(current);
    }

    let mut segments = Vec::new();
    for block in blocks {
        let Some(at) = block.iter().position(|line| line.contains("-->")) else {
            continue;
        };
        let Some((start, end)) = parse_range(block[at]) else {
            continue;
        };

        let text = block[at + 1..].join("
").trim().to_string();
        // `to_srt` writes no cue for an empty segment; reading one back would
        // add a subtitle that was deliberately not written.
        if text.is_empty() {
            continue;
        }

        segments.push(crate::domain::Segment {
            id: crate::domain::SegmentId(segments.len() as u64),
            start,
            end,
            text,
        });
    }

    if segments.is_empty() {
        return None;
    }

    Some(Transcript {
        segments,
        language: None,
    })
}

/// Both timestamps on a cue line, ignoring anything after the second.
fn parse_range(line: &str) -> Option<(Duration, Duration)> {
    let (from, to) = line.split_once("-->")?;

    // A cue may carry settings after the end timestamp, e.g.
    // `00:00:05,000 align:start position:10%`. The first word is the time.
    let to = to.split_whitespace().next()?;

    Some((parse_timestamp(from.trim())?, parse_timestamp(to)?))
}

/// `HH:MM:SS,mmm`, or with a dot.
fn parse_timestamp(text: &str) -> Option<Duration> {
    let (hms, millis) = text.split_once([',', '.'])?;

    let mut parts = hms.split(':');
    let hours: u64 = parts.next()?.trim().parse().ok()?;
    let minutes: u64 = parts.next()?.trim().parse().ok()?;
    let seconds: u64 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() {
        return None;
    }

    let millis: u64 = millis.trim().parse().ok()?;

    // Checked rather than trusting the file: these numbers come from text
    // somebody may have edited, and an overflow would panic in release.
    let total = hours
        .checked_mul(3_600_000)?
        .checked_add(minutes.checked_mul(60_000)?)?
        .checked_add(seconds.checked_mul(1_000)?)?
        .checked_add(millis)?;

    Some(Duration::from_millis(total))
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
    fn what_is_rendered_can_be_read_back() {
        // The property that matters: the file on disk is the result, and
        // showing it again must show the same thing.
        let original = transcript(vec![
            segment(0, 0, 1_500, "开放时间"),
            segment(1, 1_500, 3_200, "早上九点。"),
        ]);

        let text = to_srt(&original);
        let read = parse_srt(&text).expect("it is an SRT");

        assert_eq!(read.segments.len(), 2);
        assert_eq!(read.segments[0].start, Duration::ZERO);
        assert_eq!(read.segments[0].end, Duration::from_millis(1_500));
        assert_eq!(read.segments[0].text, "开放时间");
        assert_eq!(read.segments[1].text, "早上九点。");

        // And rendering it again gives the same document, which is the whole
        // of "cannot disagree about the format".
        assert_eq!(to_srt(&read), text);
    }

    #[test]
    fn an_empty_transcript_round_trips_rather_than_failing() {
        let empty = transcript(Vec::new());
        assert_eq!(to_srt(&empty), "");
        assert_eq!(parse_srt("").expect("blank is an empty transcript").segments.len(), 0);
    }

    #[test]
    fn a_file_that_is_not_subtitles_is_refused_rather_than_half_read() {
        // No cue anywhere: this is somebody's notes, not a transcript, and
        // treating its lines as subtitles would put them on screen with no
        // timestamps and call it a result.
        assert!(parse_srt("这是一段普通文字。
第二行。").is_none());
    }

    #[test]
    fn the_things_a_player_tolerates_are_tolerated_here() {
        // Cue numbers out of order and repeated, a dot instead of a comma, a
        // position setting after the end timestamp, and a leading block that
        // is not a cue at all. Players accept all of it; refusing it would
        // refuse a file that works.
        let text = "WEBVTT-ish header

                    7
00:00:00.000 --> 00:00:01,000 align:start
第一条

                    7
00:00:01,000 --> 00:00:02.500
第二条
";

        let read = parse_srt(text).expect("this is readable");

        assert_eq!(read.segments.len(), 2, "the header is not a cue");
        assert_eq!(read.segments[0].text, "第一条");
        assert_eq!(read.segments[0].start, Duration::ZERO);
        assert_eq!(read.segments[1].end, Duration::from_millis(2_500));
    }

    #[test]
    fn a_text_line_before_the_first_cue_does_not_shift_the_timestamps() {
        // The cue number is a position, not information, so it is not counted
        // — a file whose numbers were renumbered by hand must read the same.
        let read = parse_srt("1
00:00:00,000 --> 00:00:01,000
甲
").expect("one cue");
        assert_eq!(read.segments[0].text, "甲");

        let renumbered = parse_srt("99
00:00:00,000 --> 00:00:01,000
甲
").expect("one cue");
        assert_eq!(renumbered.segments[0].text, "甲");
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
