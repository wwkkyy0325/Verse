//! Character error rate.
//!
//! CER is `(substitutions + deletions + insertions) / reference length`, over
//! characters. For Chinese that means one unit per 汉字, which is why the
//! normalisation below is not optional: a stray full stop is a character, and
//! a recogniser that punctuates will look worse than one that does not for a
//! reason that has nothing to do with recognition.

/// Strip everything that is not a character of speech, and case-fold.
///
/// Deliberately blunt. The reference transcripts in the datasets used here are
/// bare 汉字 with no punctuation and no Latin, while the recogniser emits
/// punctuated, normalised text — so comparing them raw measures punctuation
/// agreement rather than recognition. Punctuation is a separate concern with
/// its own model and its own tests; it does not belong inside a CER.
///
/// What is *kept*: 汉字, Latin letters and digits. What is dropped: whitespace
/// and every mark that is not a letter or a number in any script.
pub fn normalize(text: &str) -> Vec<char> {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Levenshtein distance, counted in characters.
///
/// Two rolling rows rather than a full matrix: the inputs here run to a few
/// hundred characters, and the report is over thousands of them.
pub fn distance(reference: &[char], hypothesis: &[char]) -> usize {
    if reference.is_empty() {
        return hypothesis.len();
    }

    let mut previous: Vec<usize> = (0..=hypothesis.len()).collect();
    let mut current = vec![0usize; hypothesis.len() + 1];

    for (i, r) in reference.iter().enumerate() {
        current[0] = i + 1;

        for (j, h) in hypothesis.iter().enumerate() {
            let substitution = previous[j] + usize::from(r != h);
            let deletion = previous[j + 1] + 1;
            let insertion = current[j] + 1;
            current[j + 1] = substitution.min(deletion).min(insertion);
        }

        std::mem::swap(&mut previous, &mut current);
    }

    previous[hypothesis.len()]
}

/// One scored utterance.
pub struct Score {
    pub errors: usize,
    pub reference_len: usize,
}

impl Score {
    pub fn rate(&self) -> f64 {
        if self.reference_len == 0 {
            return 0.0;
        }
        self.errors as f64 / self.reference_len as f64
    }
}

/// Score one hypothesis against its reference.
pub fn score(reference: &str, hypothesis: &str) -> Score {
    let reference = normalize(reference);
    let hypothesis = normalize(hypothesis);

    Score {
        errors: distance(&reference, &hypothesis),
        reference_len: reference.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exact_match_is_free() {
        let s = score("甚至出现交易几乎停滞的情况", "甚至出现交易几乎停滞的情况");
        assert_eq!(s.errors, 0);
        assert_eq!(s.rate(), 0.0);
    }

    #[test]
    fn punctuation_and_spacing_do_not_count() {
        // The recogniser punctuates; the reference does not. If this were an
        // error, every dataset here would report a rate nobody could act on.
        let s = score("甚至出现交易几乎停滞的情况", "甚至出现交易，几乎停滞的情况。");
        assert_eq!(s.errors, 0);
    }

    #[test]
    fn one_wrong_character_is_one_error() {
        let s = score("开放时间", "开饭时间");
        assert_eq!(s.errors, 1);
        assert_eq!(s.reference_len, 4);
        assert!((s.rate() - 0.25).abs() < 1e-9);
    }

    #[test]
    fn a_dropped_character_is_an_error() {
        let s = score("开放时间", "开时间");
        assert_eq!(s.errors, 1);
    }

    #[test]
    fn an_added_character_is_an_error() {
        // The `Tuesdayesday` failure: the recogniser repeats a run of letters.
        // `tuesday` is seven characters and the output is twelve, so the cost
        // is the five that were inserted.
        let s = score("tuesday", "tuesdayesday");
        assert_eq!(s.errors, 5);
    }

    #[test]
    fn latin_case_is_ignored() {
        let s = score("Tuesday", "tuesday");
        assert_eq!(s.errors, 0);
    }

    #[test]
    fn digits_and_letters_survive_normalisation() {
        assert_eq!(normalize("9点abc"), vec!['9', '点', 'a', 'b', 'c']);
    }

    #[test]
    fn an_empty_hypothesis_costs_the_whole_reference() {
        let s = score("开放时间", "");
        assert_eq!(s.errors, 4);
        assert_eq!(s.rate(), 1.0);
    }

    #[test]
    fn an_empty_reference_scores_zero_rather_than_dividing_by_it() {
        let s = score("", "anything");
        assert_eq!(s.rate(), 0.0);
    }
}
