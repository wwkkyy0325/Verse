//! Measuring how much of a file the segmenter actually kept.
//!
//! The pipeline is the only place that sees both the audio the decoder
//! produced and the audio the segmenter chose to pass on, and until now it
//! discarded the relationship. That relationship is the whole question: the
//! recogniser cannot transcribe audio it was never given, and nothing
//! downstream can tell a short recording from a truncated one.
//!
//! Three quantities, all in samples of the *decoded* stream:
//!
//! - **decoded** — everything the decoder produced.
//! - **voiced** — the union of the spans' time ranges. Merged, not summed:
//!   spans carry leading padding the segmenter adds for context, so they
//!   overlap, and summing would credit the same audio more than once. An
//!   inflated `voiced` makes the guard fire less often, which is the wrong
//!   direction to be wrong in.
//! - **energetic** — the audio that is *not silence*. Judged against the
//!   file's own loudest window rather than an absolute level, because the
//!   question is "was there sound here the detector ignored", not "was this
//!   recording loud". A quiet recording is still a recording.

use std::time::Duration;

/// Analysis window, in seconds.
///
/// Long enough for a stable level estimate, short enough that a pause inside a
/// sentence is visible.
const WINDOW_SECONDS: f64 = 0.1;

/// How far below the file's own loudest window a window may sit and still
/// count as carrying sound.
///
/// Relative, so it does not depend on how the recording was levelled. 25 dB is
/// a wide margin: speech within a single recording varies by far less, while a
/// genuine silence sits far below.
const ENERGY_RANGE_DB: f32 = 25.0;

/// Below this the file is silence, whatever the relative measure says.
///
/// Without it a digitally silent file has a peak of zero, so every window
/// clears a floor of zero and the file reads as pure speech — the exact
/// inversion of the truth. -60 dBFS is far below any recording anyone would
/// ask to transcribe.
const SILENCE_PEAK_RMS: f32 = 1e-3;

/// What the segmenter did with one file.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Coverage {
    pub decoded_seconds: f64,
    pub voiced_seconds: f64,
    pub energetic_seconds: f64,
}

impl Coverage {
    /// Fraction of the file's non-silent audio that reached the recogniser.
    ///
    /// `None` when there was not enough sound to judge — a silent file is not
    /// a failure, and reporting 0% for one would be a false alarm.
    pub fn ratio(&self) -> Option<f64> {
        if self.energetic_seconds <= 0.0 {
            return None;
        }
        Some((self.voiced_seconds / self.energetic_seconds).min(1.0))
    }
}

/// Below this fraction of a file's non-silent audio, the segmenter's reading
/// is not believed.
///
/// Chosen by sweeping it over all 898 conversation utterances, the way the VAD
/// threshold was. At 0.70 nine files fire and **every one of them improves**,
/// with the worst going from 15 errors in 16 characters to 1. The mean rate
/// falls 0.32 points.
///
/// Higher floors recover more — 0.97 fires 44 times and gains 0.56 points —
/// but they also start damaging files that were already right, by a character
/// or two each. The first such file sits at 0.730, so 0.70 is the highest
/// round value with any margin, and a guard whose whole justification is that
/// it is safe to be wrong should not be tuned to the last thousandth.
///
/// **Coverage is a measure of damage, not of failure.** Of the eleven files
/// known to defeat the detector, only four have low coverage; the rest kept
/// nearly all their audio and were nevertheless mis-recognised. This catches
/// the losses, which are the catastrophic ones, and does not catch the rest.
pub const DEFAULT_FLOOR: f64 = 0.70;

/// Files holding less sound than this are not judged.
///
/// A fraction of a second is not enough to tell a mis-detection from a short
/// clip, and re-recognising it would cost more than it returns.
pub const DEFAULT_MIN_ENERGETIC_SECONDS: f64 = 1.0;

/// When to distrust the segmenter.
#[derive(Debug, Clone, Copy)]
pub struct GuardSettings {
    /// Fraction of non-silent audio the segmenter must keep before its output
    /// is believed. Zero never fires, which is how the guard is turned off.
    pub floor: f64,
    /// Least non-silent audio a file must hold to be judged at all.
    pub min_energetic_seconds: f64,
}

impl Default for GuardSettings {
    fn default() -> Self {
        Self {
            floor: DEFAULT_FLOOR,
            min_energetic_seconds: DEFAULT_MIN_ENERGETIC_SECONDS,
        }
    }
}

impl GuardSettings {
    /// Whether this reading is implausible enough to act on.
    ///
    /// Deliberately answers `false` when there is any doubt: the guard trades
    /// a rare silent failure for a re-run, and a re-run is only cheap while it
    /// stays rare.
    pub fn should_recover(&self, coverage: &Coverage) -> bool {
        if self.floor <= 0.0 {
            return false;
        }
        if coverage.energetic_seconds < self.min_energetic_seconds {
            return false;
        }
        match coverage.ratio() {
            Some(ratio) => ratio < self.floor,
            // Nothing to judge: a file with no sound is not a failure.
            None => false,
        }
    }
}

/// Accumulates [`Coverage`] as a file is decoded and segmented.
pub struct CoverageMeter {
    rate: usize,
    window_samples: usize,
    /// Root-mean-square of each analysis window, with its length.
    ///
    /// The last window is usually short, which is why the length is kept.
    levels: Vec<(f32, usize)>,
    /// Samples of the window still being filled.
    pending: Vec<f32>,
    decoded: usize,
    /// End of the merged span ranges so far, as a sample index.
    covered_until: usize,
    voiced: usize,
}

impl CoverageMeter {
    pub fn new(sample_rate: u32) -> Self {
        let rate = sample_rate.max(1) as usize;
        Self {
            rate,
            window_samples: (WINDOW_SECONDS * rate as f64).round().max(1.0) as usize,
            levels: Vec::new(),
            pending: Vec::new(),
            decoded: 0,
            covered_until: 0,
            voiced: 0,
        }
    }

    /// Record audio handed to the segmenter.
    ///
    /// Whole windows are taken by cursor and the remainder compacted once, so
    /// the cost does not depend on how large a chunk the decoder happens to
    /// deliver. Draining per window would be quadratic in the chunk size, and
    /// the chunk size belongs to the decoder, not to this.
    pub fn observe(&mut self, samples: &[f32]) {
        self.decoded += samples.len();
        self.pending.extend_from_slice(samples);

        let mut taken = 0;
        while self.pending.len() - taken >= self.window_samples {
            let window = &self.pending[taken..taken + self.window_samples];
            self.levels.push((root_mean_square(window), self.window_samples));
            taken += self.window_samples;
        }

        if taken > 0 {
            self.pending.drain(..taken);
        }
    }

    /// Record a span the segmenter produced.
    ///
    /// Spans arrive in order, which is what lets the merge be a running high
    /// water mark rather than a list of intervals.
    pub fn kept(&mut self, start: Duration, samples: usize) {
        let from = (start.as_secs_f64() * self.rate as f64).round() as usize;
        let to = from + samples;

        let from = from.max(self.covered_until);
        if to > from {
            self.voiced += to - from;
            self.covered_until = to;
        }
    }

    pub fn finish(mut self) -> Coverage {
        if !self.pending.is_empty() {
            let length = self.pending.len();
            let rms = root_mean_square(&self.pending);
            self.levels.push((rms, length));
        }

        let peak = self
            .levels
            .iter()
            .map(|(rms, _)| *rms)
            .fold(0.0f32, f32::max);

        let energetic = if peak < SILENCE_PEAK_RMS {
            0
        } else {
            let floor = peak * 10f32.powf(-ENERGY_RANGE_DB / 20.0);
            self.levels
                .iter()
                .filter(|(rms, _)| *rms >= floor)
                .map(|(_, length)| *length)
                .sum()
        };

        let seconds = |samples: usize| samples as f64 / self.rate as f64;

        Coverage {
            decoded_seconds: seconds(self.decoded),
            voiced_seconds: seconds(self.voiced),
            energetic_seconds: seconds(energetic),
        }
    }
}

fn root_mean_square(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    fn speech(seconds: f64, amplitude: f32) -> Vec<f32> {
        // A tone rather than noise: it has a stable, predictable level.
        let n = (seconds * RATE as f64) as usize;
        (0..n)
            .map(|i| amplitude * (i as f32 * 0.1).sin())
            .collect()
    }

    fn meter_over(samples: &[f32]) -> CoverageMeter {
        let mut meter = CoverageMeter::new(RATE);
        meter.observe(samples);
        meter
    }

    #[test]
    fn a_silent_file_has_no_energetic_audio() {
        let coverage = meter_over(&vec![0.0f32; RATE as usize]).finish();
        assert_eq!(coverage.energetic_seconds, 0.0);
        assert_eq!(coverage.ratio(), None);
    }

    #[test]
    fn speech_throughout_is_entirely_energetic() {
        let coverage = meter_over(&speech(2.0, 0.3)).finish();
        assert!(
            (coverage.energetic_seconds - 2.0).abs() < 0.11,
            "{:?}",
            coverage
        );
    }

    #[test]
    fn a_quiet_tail_is_not_energetic() {
        // One second of speech, then two seconds 40 dB down. The quiet part is
        // sound in the file but not sound the detector should have claimed.
        let mut audio = speech(1.0, 0.3);
        audio.extend(speech(2.0, 0.003));

        let coverage = meter_over(&audio).finish();
        assert!(
            (coverage.energetic_seconds - 1.0).abs() < 0.11,
            "{:?}",
            coverage
        );
    }

    #[test]
    fn a_quiet_recording_is_still_a_recording() {
        // The same shape, 40 dB quieter overall. The ratio is unchanged
        // because the measure is relative to the file's own peak.
        let loud = meter_over(&speech(1.0, 0.3)).finish();
        let soft = meter_over(&speech(1.0, 0.003)).finish();
        assert!((loud.energetic_seconds - soft.energetic_seconds).abs() < 0.11);
    }

    #[test]
    fn kept_spans_are_merged_not_summed() {
        let mut meter = meter_over(&speech(3.0, 0.3));
        // Two spans overlapping by half a second, as the segmenter's leading
        // padding produces. Summing would report 2.0.
        meter.kept(Duration::from_millis(0), RATE as usize);
        meter.kept(Duration::from_millis(500), RATE as usize);

        let coverage = meter.finish();
        assert!(
            (coverage.voiced_seconds - 1.5).abs() < 0.01,
            "{:?}",
            coverage
        );
    }

    #[test]
    fn a_span_entirely_inside_another_adds_nothing() {
        // Two seconds claimed, then one second claimed again from inside that
        // range — as padding produces when a span is re-covered.
        let mut meter = meter_over(&speech(3.0, 0.3));
        meter.kept(Duration::from_millis(0), RATE as usize * 2);
        meter.kept(Duration::from_millis(1000), RATE as usize / 2);

        let coverage = meter.finish();
        assert!(
            (coverage.voiced_seconds - 2.0).abs() < 0.01,
            "{:?}",
            coverage
        );
    }

    #[test]
    fn separate_spans_are_counted_separately() {
        // A gap between spans is audio the segmenter did not keep, and the
        // measure has to show it.
        let mut meter = meter_over(&speech(3.0, 0.3));
        meter.kept(Duration::from_millis(0), RATE as usize);
        meter.kept(Duration::from_millis(2000), RATE as usize);

        let coverage = meter.finish();
        assert!(
            (coverage.voiced_seconds - 2.0).abs() < 0.01,
            "{:?}",
            coverage
        );
    }

    #[test]
    fn the_ratio_reports_what_was_kept() {
        // Five seconds of speech, one second of it handed on.
        let mut meter = meter_over(&speech(5.0, 0.3));
        meter.kept(Duration::from_millis(0), RATE as usize);

        let coverage = meter.finish();
        assert!((coverage.ratio().unwrap() - 0.2).abs() < 0.03, "{:?}", coverage);
    }

    #[test]
    fn the_ratio_never_exceeds_one() {
        // A segmenter cannot keep more than it was given, but padding means it
        // can appear to; the ratio is clamped rather than reporting nonsense.
        let mut meter = meter_over(&speech(1.0, 0.3));
        meter.kept(Duration::from_millis(0), RATE as usize * 4);

        assert!((meter.finish().ratio().unwrap() - 1.0).abs() < 0.01);
    }

    fn reading(voiced: f64, energetic: f64) -> Coverage {
        Coverage {
            decoded_seconds: energetic,
            voiced_seconds: voiced,
            energetic_seconds: energetic,
        }
    }

    #[test]
    fn a_file_the_segmenter_dropped_is_recovered() {
        // The measured case: five seconds of sound, under two kept.
        assert!(GuardSettings::default().should_recover(&reading(1.43, 5.0)));
    }

    #[test]
    fn a_file_the_segmenter_kept_is_left_alone() {
        assert!(!GuardSettings::default().should_recover(&reading(5.0, 5.0)));
        assert!(!GuardSettings::default().should_recover(&reading(4.5, 5.0)));
    }

    #[test]
    fn a_silent_file_is_never_recovered() {
        // Nothing was there to lose, and re-running would be pure cost.
        let silent = Coverage {
            decoded_seconds: 30.0,
            voiced_seconds: 0.0,
            energetic_seconds: 0.0,
        };
        assert!(!GuardSettings::default().should_recover(&silent));
    }

    #[test]
    fn a_clip_too_short_to_judge_is_left_alone() {
        // Half a second of sound, a third of it kept — a real ratio, but not
        // enough audio for the reading to mean anything.
        assert!(!GuardSettings::default().should_recover(&reading(0.16, 0.5)));
    }

    #[test]
    fn a_floor_of_zero_turns_the_guard_off() {
        let off = GuardSettings {
            floor: 0.0,
            ..GuardSettings::default()
        };
        assert!(!off.should_recover(&reading(0.0, 60.0)));
    }
}
