//! Reporting.
//!
//! A single average across a corpus is a number that hides where the failures
//! are. The first run of this tool averaged AISHELL — 400 speakers reading
//! news in a quiet room — and reported 8.44%, which was wrong twice over: it
//! counted formatting as error, and it described a condition the product is
//! never actually used in.
//!
//! So results are always reported per dataset, and the summary line exists to
//! be scanned rather than quoted.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::cer;
use crate::punct;

/// One scored utterance.
pub struct Scored {
    pub id: String,
    pub reference: String,
    pub hypothesis: String,
    pub score: cer::Score,
}

impl Scored {
    pub fn rate(&self) -> f64 {
        self.score.rate()
    }
}

/// Everything scored for one dataset.
pub struct Dataset {
    pub name: String,
    pub results: Vec<Scored>,
    pub errors: usize,
    pub characters: usize,
    pub by_mark: BTreeMap<char, punct::MarkScore>,
    pub punctuation: punct::MarkScore,
    /// Whether the references carry punctuation at all.
    ///
    /// A punctuation score against bare text is not a low score, it is a
    /// meaningless one, and printing 0% next to it would read as a failure
    /// rather than as a question that dataset cannot answer.
    pub scored_punctuation: bool,
    pub elapsed_seconds: f64,
}

impl Dataset {
    pub fn rate(&self) -> f64 {
        if self.characters == 0 {
            0.0
        } else {
            self.errors as f64 / self.characters as f64
        }
    }

    pub fn exact(&self) -> usize {
        self.results.iter().filter(|r| r.score.errors == 0).count()
    }
}

/// The one-line-per-dataset table.
pub fn summary(datasets: &[Dataset]) {
    println!();
    println!(
        "{:<22} {:>7} {:>8} {:>9} {:>10} {:>10}",
        "dataset", "utter", "CER", "exact", "punct F1", "time"
    );
    println!("{}", "-".repeat(70));

    for dataset in datasets {
        let punctuation = if dataset.scored_punctuation {
            format!("{:.1}%", dataset.punctuation.f1() * 100.0)
        } else {
            "—".to_string()
        };

        println!(
            "{:<22} {:>7} {:>8} {:>9} {:>10} {:>9.0}s",
            dataset.name,
            dataset.results.len(),
            format!("{:.2}%", dataset.rate() * 100.0),
            format!(
                "{:.1}%",
                100.0 * dataset.exact() as f64 / dataset.results.len().max(1) as f64
            ),
            punctuation,
            dataset.elapsed_seconds,
        );
    }
}

/// The full account of one dataset.
pub fn detail(dataset: &Dataset, worst_shown: usize) {
    println!();
    println!("dataset         {}", dataset.name);
    println!("utterances      {}", dataset.results.len());
    println!("reference chars {}", dataset.characters);
    println!("errors          {}", dataset.errors);
    println!("CER             {:.2}%", dataset.rate() * 100.0);
    println!("wall time       {:.1}s", dataset.elapsed_seconds);
    println!(
        "exact matches   {} ({:.1}%)",
        dataset.exact(),
        100.0 * dataset.exact() as f64 / dataset.results.len().max(1) as f64
    );

    if dataset.results.is_empty() {
        println!();
        println!("nothing was scored — every file failed before recognition.");
        return;
    }

    // A mean hides the shape. If most utterances are perfect and a few are
    // catastrophic, the fix is somewhere very different than if everything is
    // mildly wrong.
    let mut rates: Vec<f64> = dataset.results.iter().map(|r| r.rate()).collect();
    rates.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let at = |p: f64| rates[((rates.len() - 1) as f64 * p) as usize] * 100.0;

    println!();
    println!(
        "distribution    p50 {:.1}%   p90 {:.1}%   p99 {:.1}%",
        at(0.50),
        at(0.90),
        at(0.99)
    );

    punctuation_detail(dataset);

    let mut worst: Vec<&Scored> = dataset.results.iter().collect();
    worst.sort_by(|a, b| {
        b.score
            .errors
            .cmp(&a.score.errors)
            .then(b.score.reference_len.cmp(&a.score.reference_len))
    });

    println!();
    println!("worst {worst_shown}:");
    for item in worst.iter().take(worst_shown) {
        println!();
        println!(
            "  [{}] {} errors / {} chars",
            item.id, item.score.errors, item.score.reference_len
        );
        println!("   ref  {}", item.reference);
        println!("   got  {}", item.hypothesis);
    }
}

/// Punctuation, scored separately from recognition.
///
/// Reported beside the character rate rather than folded into it, because the
/// two answer different questions and pull in opposite directions: a
/// recogniser can be accurate and unreadable, and this one was chosen for
/// being readable.
pub fn punctuation_detail(dataset: &Dataset) {
    if !dataset.scored_punctuation {
        return;
    }

    println!();
    println!("punctuation");
    println!("  mark      prec     rec      F1      TP     FP     FN");

    for (mark, score) in &dataset.by_mark {
        println!(
            "  {mark}      {:6.1}%  {:6.1}%  {:6.1}%  {:6} {:6} {:6}",
            score.precision() * 100.0,
            score.recall() * 100.0,
            score.f1() * 100.0,
            score.true_positives,
            score.false_positives,
            score.false_negatives,
        );
    }

    let micro = &dataset.punctuation;
    println!(
        "  overall   {:6.1}%  {:6.1}%  {:6.1}%  {:6} {:6} {:6}",
        micro.precision() * 100.0,
        micro.recall() * 100.0,
        micro.f1() * 100.0,
        micro.true_positives,
        micro.false_positives,
        micro.false_negatives,
    );
}

/// Write every pair out, so a subset can be read without re-running.
pub fn dump(path: &std::path::Path, results: &[Scored]) -> std::io::Result<()> {
    let mut out = String::new();
    for item in results {
        writeln!(
            out,
            "{}\t{}\t{}\t{}",
            item.id, item.score.errors, item.reference, item.hypothesis
        )
        .ok();
    }
    std::fs::write(path, out)
}
