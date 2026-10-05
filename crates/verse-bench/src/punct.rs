//! Punctuation scoring.
//!
//! Character error rate drops punctuation entirely, which is the right thing
//! for measuring recognition and the wrong thing for measuring subtitles: a
//! transcript with no punctuation is unreadable as SRT, and it is the reason
//! SenseVoice was chosen over an engine that needs a second 294 MB model to
//! punctuate. Scoring it off a character rate would make that decision
//! invisible.
//!
//! The measure is the one used in the punctuation-restoration literature:
//! **precision, recall and F1 per mark**, plus a micro-average over all of
//! them. A mark is scored by *where* it appears, not whether it appears at
//! all — 你好，世界 and 你好。世界 both contain one mark and mean different
//! things.
//!
//! That makes position the hard part. The two texts are different lengths and
//! disagree about some characters, so marks cannot simply be compared by
//! index. The characters are aligned first (by edit distance), and marks are
//! only compared where the underlying character matched. A mark attached to a
//! character the recogniser got wrong is neither credited nor blamed, because
//! there is no way to say where it should have gone.

use std::collections::BTreeMap;

/// Marks worth scoring, and the only ones counted.
///
/// Restricted on purpose. A recogniser that emits 「」 or …… somewhere harmless
/// should not be penalised for it, and in practice these four carry almost all
/// of the readability. Anything else is dropped from both sides before
/// scoring, which keeps precision honest: an unlisted mark cannot produce a
/// false positive.
const SCORED: [char; 4] = ['，', '。', '？', '！'];

/// A character and the mark that follows it, if any.
type Token = (char, Option<char>);

/// Split text into characters with their trailing mark.
///
/// Marks that are not scored are dropped, and a run of them contributes one
/// mark: `好。」` and `好。` are the same boundary as far as readability goes.
fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens: Vec<Token> = Vec::new();

    for ch in text.chars() {
        if SCORED.contains(&ch) {
            if let Some(last) = tokens.last_mut() {
                // First scored mark after a character wins; a second one is
                // the same boundary said twice.
                if last.1.is_none() {
                    last.1 = Some(ch);
                }
                continue;
            }
        } else if ch.is_alphanumeric() {
            tokens.push((ch, None));
            continue;
        }
        // Whitespace and unscored marks are dropped entirely.
    }

    tokens
}

/// An alignment between two character sequences.
///
/// `None` marks a gap — a character present on one side and not the other.
struct Alignment {
    pairs: Vec<(Option<char>, Option<char>)>,
}

/// Align two character sequences by edit distance.
///
/// Full matrix rather than rolling rows, because the path is needed and not
/// just its cost. The sequences here are a few hundred characters; this is
/// not the expensive part of the run.
fn align(reference: &[char], hypothesis: &[char]) -> Alignment {
    let n = reference.len();
    let m = hypothesis.len();

    let mut cost = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in cost.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in cost[0].iter_mut().enumerate() {
        *cell = j;
    }

    for i in 1..=n {
        for j in 1..=m {
            let substitution = cost[i - 1][j - 1] + usize::from(reference[i - 1] != hypothesis[j - 1]);
            let deletion = cost[i - 1][j] + 1;
            let insertion = cost[i][j - 1] + 1;
            cost[i][j] = substitution.min(deletion).min(insertion);
        }
    }

    // Walk back from the end. A diagonal step is taken whenever it explains
    // the cost — whether it was a match or a substitution — so identical
    // characters line up instead of being spent on a gap. Which of the two it
    // was does not matter here: the scorer compares the characters itself.
    let mut pairs = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        let diagonal = i > 0
            && j > 0
            && cost[i][j] == cost[i - 1][j - 1] + usize::from(reference[i - 1] != hypothesis[j - 1]);

        if diagonal {
            pairs.push((Some(reference[i - 1]), Some(hypothesis[j - 1])));
            i -= 1;
            j -= 1;
        } else if i > 0 && cost[i][j] == cost[i - 1][j] + 1 {
            pairs.push((Some(reference[i - 1]), None));
            i -= 1;
        } else {
            pairs.push((None, Some(hypothesis[j - 1])));
            j -= 1;
        }
    }

    pairs.reverse();
    Alignment { pairs }
}

/// How one mark did.
#[derive(Debug, Clone, Copy, Default)]
pub struct MarkScore {
    pub true_positives: usize,
    pub false_positives: usize,
    pub false_negatives: usize,
}

impl MarkScore {
    pub fn precision(&self) -> f64 {
        let denominator = self.true_positives + self.false_positives;
        if denominator == 0 {
            // Nothing was predicted. That is not 100% precision, it is no
            // evidence either way; reporting 1.0 would flatter a silent model.
            return 0.0;
        }
        self.true_positives as f64 / denominator as f64
    }

    pub fn recall(&self) -> f64 {
        let denominator = self.true_positives + self.false_negatives;
        if denominator == 0 {
            return 0.0;
        }
        self.true_positives as f64 / denominator as f64
    }

    pub fn f1(&self) -> f64 {
        let (p, r) = (self.precision(), self.recall());
        if p + r == 0.0 {
            return 0.0;
        }
        2.0 * p * r / (p + r)
    }
}

/// Every mark's score, and the micro-average across them.
pub struct PunctuationScore {
    pub by_mark: BTreeMap<char, MarkScore>,
    pub micro: MarkScore,
}

impl PunctuationScore {
    /// The marks that were actually seen, in a fixed order.
    pub fn rows(&self) -> Vec<(char, MarkScore)> {
        SCORED
            .iter()
            .filter_map(|mark| self.by_mark.get(mark).map(|score| (*mark, *score)))
            .collect()
    }
}

/// Score the punctuation of `hypothesis` against `reference`.
pub fn score(reference: &str, hypothesis: &str) -> PunctuationScore {
    let reference = tokenize(reference);
    let hypothesis = tokenize(hypothesis);

    let reference_chars: Vec<char> = reference.iter().map(|(ch, _)| *ch).collect();
    let hypothesis_chars: Vec<char> = hypothesis.iter().map(|(ch, _)| *ch).collect();

    // The alignment is over characters; the marks ride along on them.
    let alignment = align(&reference_chars, &hypothesis_chars);

    // Index into each token list as the walk proceeds, so marks can be looked
    // up for the characters the alignment decided matched.
    let mut by_mark: BTreeMap<char, MarkScore> = BTreeMap::new();
    let mut micro = MarkScore::default();

    let (mut r, mut h) = (0usize, 0usize);
    for (reference_ch, hypothesis_ch) in &alignment.pairs {
        let reference_mark = if reference_ch.is_some() {
            let mark = reference[r].1;
            r += 1;
            mark
        } else {
            None
        };
        let hypothesis_mark = if hypothesis_ch.is_some() {
            let mark = hypothesis[h].1;
            h += 1;
            mark
        } else {
            None
        };

        // Only positions where both sides agree on the character can be
        // scored. Elsewhere there is no shared anchor for a mark.
        if reference_ch != hypothesis_ch {
            continue;
        }

        match (reference_mark, hypothesis_mark) {
            (Some(a), Some(b)) if a == b => {
                by_mark.entry(a).or_default().true_positives += 1;
                micro.true_positives += 1;
            }
            (Some(a), Some(b)) => {
                by_mark.entry(a).or_default().false_negatives += 1;
                by_mark.entry(b).or_default().false_positives += 1;
                micro.false_negatives += 1;
                micro.false_positives += 1;
            }
            (Some(a), None) => {
                by_mark.entry(a).or_default().false_negatives += 1;
                micro.false_negatives += 1;
            }
            (None, Some(b)) => {
                by_mark.entry(b).or_default().false_positives += 1;
                micro.false_positives += 1;
            }
            (None, None) => {}
        }
    }

    PunctuationScore { by_mark, micro }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_attaches_a_mark_to_the_character_before_it() {
        let tokens = tokenize("你好，世界。");
        assert_eq!(
            tokens,
            vec![('你', None), ('好', Some('，')), ('世', None), ('界', Some('。'))]
        );
    }

    #[test]
    fn a_run_of_marks_counts_once() {
        // `好。」` is one boundary, not three.
        let tokens = tokenize("好。」");
        assert_eq!(tokens, vec![('好', Some('。'))]);
    }

    #[test]
    fn unscored_marks_are_dropped_from_both_sides() {
        let tokens = tokenize("好…啊");
        assert_eq!(tokens, vec![('好', None), ('啊', None)]);
    }

    #[test]
    fn an_identical_pair_scores_a_perfect_f1() {
        let s = score("妈妈，你在哪儿呀？", "妈妈，你在哪儿呀？");
        assert_eq!(s.micro.false_positives, 0);
        assert_eq!(s.micro.false_negatives, 0);
        assert_eq!(s.micro.true_positives, 2);
        assert!((s.micro.f1() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_missing_mark_is_a_false_negative() {
        let s = score("妈妈，你在哪儿呀？", "妈妈你在哪儿呀？");
        assert_eq!(s.micro.true_positives, 1);
        assert_eq!(s.micro.false_negatives, 1);
        assert_eq!(s.micro.false_positives, 0);

        let comma = s.by_mark[&'，'];
        assert_eq!(comma.true_positives, 0);
        assert_eq!(comma.false_negatives, 1);
    }

    #[test]
    fn a_wrong_mark_is_both_a_false_positive_and_a_false_negative() {
        let s = score("你好。", "你好？");
        let period = s.by_mark[&'。'];
        let question = s.by_mark[&'？'];

        assert_eq!(period.false_negatives, 1);
        assert_eq!(question.false_positives, 1);
        assert_eq!(s.micro.true_positives, 0);
    }

    #[test]
    fn the_same_mark_in_the_wrong_place_does_not_score() {
        // The whole point of aligning: one mark each, but not the same
        // boundary, so it counts as both a miss and a false alarm.
        let s = score("你好，世界。", "你好。世界。");
        assert_eq!(s.micro.true_positives, 1); // the final 。
        assert_eq!(s.by_mark[&'，'].false_negatives, 1);
        assert_eq!(s.by_mark[&'。'].false_positives, 1);
    }

    #[test]
    fn a_mark_on_a_misrecognised_character_is_ignored() {
        // `地` was heard as `也`. The comma hangs off a character the two
        // sides disagree about, so there is no anchor to place it against and
        // it is neither credited nor blamed. Only the final `。`, on a
        // character both agree on, scores.
        let s = score("土地，很好。", "土也，很好。");
        assert_eq!(s.micro.true_positives, 1);
        assert_eq!(s.micro.false_positives, 0);
        assert_eq!(s.micro.false_negatives, 0);
    }

    #[test]
    fn predicting_nothing_scores_zero_rather_than_perfect_precision() {
        // A model that never punctuates must not look precise.
        let s = score("你好，世界。", "你好世界");
        assert_eq!(s.micro.precision(), 0.0);
        assert_eq!(s.micro.recall(), 0.0);
        assert_eq!(s.micro.f1(), 0.0);
    }

    #[test]
    fn latin_and_digits_are_characters_too() {
        let tokens = tokenize("abc，123。");
        assert_eq!(
            tokens,
            vec![
                ('a', None),
                ('b', None),
                ('c', Some('，')),
                ('1', None),
                ('2', None),
                ('3', Some('。')),
            ]
        );
    }
}
