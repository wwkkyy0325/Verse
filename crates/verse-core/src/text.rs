//! Text post-processing.
//!
//! Recognition emits raw text. Punctuation, translation and inverse text
//! normalization all act on it the same way — text in, text out. Treating them
//! as one trait is what lets a pipeline chain them without any of them knowing
//! the others exist.

use crate::domain::Transcript;

/// A text transformation stage.
///
/// # Contract
///
/// Implementations must not lose data. When a processor cannot do its job it
/// returns the input unchanged — never an empty string, never a partial
/// result. A punctuation failure should cost readability, not the transcript.
///
/// Processors hold no shared state and take no locks on each other, so chaining
/// them cannot produce interference beyond the text passed along.
pub trait TextProcessor: Send + Sync {
    /// Stable identifier, for logs, configuration and diagnostics.
    fn name(&self) -> &'static str;

    /// Transform `input`. Must return `input` unchanged if the work fails.
    fn process(&self, input: &str) -> String;
}

/// An ordered chain of [`TextProcessor`]s.
#[derive(Default)]
pub struct TextChain {
    processors: Vec<Box<dyn TextProcessor>>,
}

impl TextChain {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a stage. Order of insertion is order of execution.
    pub fn push(&mut self, processor: Box<dyn TextProcessor>) -> &mut Self {
        self.processors.push(processor);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.processors.is_empty()
    }

    pub fn len(&self) -> usize {
        self.processors.len()
    }

    /// The stages in execution order.
    pub fn names(&self) -> Vec<&'static str> {
        self.processors.iter().map(|p| p.name()).collect()
    }

    /// Run every stage in order.
    pub fn run(&self, input: &str) -> String {
        self.processors
            .iter()
            .fold(input.to_string(), |text, processor| {
                processor.process(&text)
            })
    }

    /// Apply the chain to every segment of a transcript.
    ///
    /// Timestamps are untouched — processors only ever see text.
    pub fn run_transcript(&self, transcript: &mut Transcript) {
        if self.processors.is_empty() {
            return;
        }
        for segment in &mut transcript.segments {
            segment.text = self.run(&segment.text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Segment, SegmentId};
    use std::time::Duration;

    /// Appends a marker so ordering is observable.
    struct Mark(&'static str, &'static str);

    impl TextProcessor for Mark {
        fn name(&self) -> &'static str {
            self.0
        }
        fn process(&self, input: &str) -> String {
            format!("{input}{}", self.1)
        }
    }

    /// Simulates a stage that fails and must therefore pass text through.
    struct Failing;

    impl TextProcessor for Failing {
        fn name(&self) -> &'static str {
            "failing"
        }
        fn process(&self, input: &str) -> String {
            input.to_string()
        }
    }

    #[test]
    fn chain_runs_in_insertion_order() {
        let mut chain = TextChain::new();
        chain.push(Box::new(Mark("a", "1")));
        chain.push(Box::new(Mark("b", "2")));

        assert_eq!(chain.run("x"), "x12");
        assert_eq!(chain.names(), vec!["a", "b"]);
    }

    #[test]
    fn an_empty_chain_is_the_identity() {
        assert_eq!(TextChain::new().run("unchanged"), "unchanged");
        assert!(TextChain::new().is_empty());
    }

    #[test]
    fn a_failing_stage_does_not_swallow_text() {
        let mut chain = TextChain::new();
        chain.push(Box::new(Failing));
        chain.push(Box::new(Mark("after", "!")));

        // The failing stage must not stop the ones behind it.
        assert_eq!(chain.run("text"), "text!");
    }

    #[test]
    fn transcript_processing_keeps_timestamps() {
        let mut chain = TextChain::new();
        chain.push(Box::new(Mark("mark", ".")));

        let mut transcript = Transcript {
            segments: vec![
                Segment {
                    id: SegmentId(1),
                    start: Duration::from_secs(1),
                    end: Duration::from_secs(2),
                    text: "first".to_string(),
                },
                Segment {
                    id: SegmentId(2),
                    start: Duration::from_secs(3),
                    end: Duration::from_secs(4),
                    text: "second".to_string(),
                },
            ],
            language: None,
        };

        chain.run_transcript(&mut transcript);

        assert_eq!(transcript.segments[0].text, "first.");
        assert_eq!(transcript.segments[1].text, "second.");
        assert_eq!(transcript.segments[0].start, Duration::from_secs(1));
        assert_eq!(transcript.segments[1].end, Duration::from_secs(4));
    }
}
