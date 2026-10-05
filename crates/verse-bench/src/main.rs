//! Scores the recognizer against a labelled dataset.
//!
//!     verse-bench <manifest.tsv> [--models <dir>] [--engine <id>] [--limit N]
//!
//! The manifest is `<id>\t<wav>\t<reference>`, as written by
//! `tools/asr-eval/extract.py`.
//!
//! **It runs the shipped pipeline**, not a copy of it. A benchmark that
//! transcribed files its own way would be measuring a program nobody uses —
//! and the boundary decisions in the pipeline are exactly what is under
//! examination here.
//!
//! Accuracy is reported as a character error rate, with the worst utterances
//! listed so a number can be turned into a diagnosis.

mod cer;
mod punct;

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use verse_core::{CancelToken, EventBus, JobId};
use verse_pipeline::{Request, Transcriber};

const DEFAULT_ENGINE: &str = "sensevoice";
const DEFAULT_MODELS: &str = "models";

/// How many of the worst utterances to print.
const WORST_SHOWN: usize = 20;

struct Options {
    manifest: PathBuf,
    models: PathBuf,
    engine: String,
    /// Stop after this many, for a quick look before committing to a full run.
    limit: Option<usize>,
    /// Where to write every reference/hypothesis pair, not just the worst.
    dump: Option<PathBuf>,
    /// Whether the engine may rewrite written forms.
    ///
    /// On by default, because that is what the product does and a benchmark
    /// should measure the product. Turning it off answers a different and
    /// also useful question: how much of the error rate is formatting rather
    /// than recognition. The reference transcripts in these datasets spell
    /// numbers out, so with `--no-itn` the two are written the same way.
    itn: bool,
    /// Speech threshold for the voice detector.
    vad_threshold: f32,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = match parse(std::env::args().skip(1).collect()) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(2);
        }
    };

    let utterances = read_manifest(&options.manifest)?;
    let utterances = match options.limit {
        Some(limit) => &utterances[..limit.min(utterances.len())],
        None => &utterances[..],
    };

    if utterances.is_empty() {
        return Err("the manifest has no usable rows".into());
    }

    let request = Request {
        input: PathBuf::new(),
        models_dir: options.models.clone(),
        engine: options.engine.clone(),
        vad: Request::vad_for(&options.models),
        inverse_text_normalization: options.itn,
        vad_threshold: options.vad_threshold,
    };

    eprintln!("loading {} ...", options.engine);
    let started = Instant::now();
    let mut transcriber = Transcriber::load(request)?;
    eprintln!("loaded in {:.1}s", started.elapsed().as_secs_f64());

    // The bus is required by the pipeline but nothing here listens; the
    // results come back through the return value.
    let bus = EventBus::new();
    let cancel = CancelToken::new();

    let mut results = Vec::with_capacity(utterances.len());
    let mut total_errors = 0usize;
    let mut total_chars = 0usize;
    let mut punctuation = punct::MarkScore::default();
    let mut by_mark: std::collections::BTreeMap<char, punct::MarkScore> =
        std::collections::BTreeMap::new();

    let run_started = Instant::now();

    for (index, utterance) in utterances.iter().enumerate() {
        transcriber.set_input(utterance.path.clone());

        let transcript = match transcriber.transcribe(JobId(index as u64 + 1), &bus, &cancel) {
            Ok(transcript) => transcript,
            Err(error) => {
                // A file that cannot be read at all is worth knowing about,
                // but it is not a recognition error and must not be folded
                // into the rate.
                eprintln!(
                    "  {} failed: {}",
                    utterance.id,
                    error.message()
                );
                continue;
            }
        };

        let hypothesis = transcript.to_text().replace('\n', "");
        let score = cer::score(&utterance.reference, &hypothesis);

        total_errors += score.errors;
        total_chars += score.reference_len;

        let marks = punct::score(&utterance.reference, &hypothesis);
        for (mark, one) in marks.rows() {
            let running = by_mark.entry(mark).or_default();
            running.true_positives += one.true_positives;
            running.false_positives += one.false_positives;
            running.false_negatives += one.false_negatives;
        }
        punctuation.true_positives += marks.micro.true_positives;
        punctuation.false_positives += marks.micro.false_positives;
        punctuation.false_negatives += marks.micro.false_negatives;

        results.push(Scored {
            id: utterance.id.clone(),
            reference: utterance.reference.clone(),
            hypothesis,
            score,
        });

        if (index + 1) % 200 == 0 {
            eprint!("\r  {}/{} ...", index + 1, utterances.len());
        }
    }

    let elapsed = run_started.elapsed().as_secs_f64();
    eprintln!("\r  {} scored in {:.1}s", results.len(), elapsed);

    report(&results, total_errors, total_chars, elapsed);
    report_punctuation(&by_mark, &punctuation);

    if let Some(path) = &options.dump {
        write_dump(path, &results)?;
        eprintln!("every pair written to {}", path.display());
    }

    Ok(())
}

struct Utterance {
    id: String,
    path: PathBuf,
    reference: String,
}

struct Scored {
    id: String,
    reference: String,
    hypothesis: String,
    score: cer::Score,
}

fn report(results: &[Scored], total_errors: usize, total_chars: usize, elapsed: f64) {
    let rate = if total_chars == 0 {
        0.0
    } else {
        total_errors as f64 / total_chars as f64
    };

    println!();
    println!("utterances      {}", results.len());
    println!("reference chars {total_chars}");
    println!("errors          {total_errors}");
    println!("CER             {:.2}%", rate * 100.0);
    println!("wall time       {elapsed:.1}s");

    let exact = results.iter().filter(|r| r.score.errors == 0).count();
    println!(
        "exact matches   {exact} ({:.1}%)",
        100.0 * exact as f64 / results.len().max(1) as f64
    );

    // Nothing scored at all. This happens when every file fails to load, which
    // is a configuration mistake rather than a result, and it must say so
    // instead of dividing by zero or indexing an empty list.
    if results.is_empty() {
        println!();
        println!("nothing was scored — every file failed before recognition.");
        return;
    }

    // A mean hides the shape. If most utterances are perfect and a few are
    // catastrophic, the fix is somewhere very different than if everything is
    // mildly wrong.
    let mut rates: Vec<f64> = results.iter().map(|r| r.score.rate()).collect();
    rates.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let at = |p: f64| rates[((rates.len() - 1) as f64 * p) as usize] * 100.0;

    println!();
    println!("distribution    p50 {:.1}%   p90 {:.1}%   p99 {:.1}%", at(0.50), at(0.90), at(0.99));

    let mut worst: Vec<&Scored> = results.iter().collect();
    worst.sort_by(|a, b| b.score.errors.cmp(&a.score.errors).then(b.score.reference_len.cmp(&a.score.reference_len)));

    println!();
    println!("worst {WORST_SHOWN}:");
    for item in worst.iter().take(WORST_SHOWN) {
        println!();
        println!("  [{}] {} errors / {} chars", item.id, item.score.errors, item.score.reference_len);
        println!("   ref  {}", item.reference);
        println!("   got  {}", item.hypothesis);
    }
}

/// Punctuation, scored separately from recognition.
///
/// Reported beside the character rate rather than folded into it, because the
/// two answer different questions and pull in opposite directions: a
/// recogniser can be accurate and unreadable, and the engine this project
/// chose was chosen for being readable.
fn report_punctuation(
    by_mark: &std::collections::BTreeMap<char, punct::MarkScore>,
    micro: &punct::MarkScore,
) {
    println!();
    println!("punctuation");
    println!("  mark      prec     rec      F1      TP     FP     FN");

    for (mark, score) in by_mark {
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

fn write_dump(path: &PathBuf, results: &[Scored]) -> std::io::Result<()> {
    let mut out = String::new();
    for item in results {
        writeln!(out, "{}\t{}\t{}\t{}", item.id, item.score.errors, item.reference, item.hypothesis)
            .ok();
    }
    std::fs::write(path, out)
}

fn read_manifest(path: &PathBuf) -> Result<Vec<Utterance>, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)?;
    let root = path.parent().unwrap_or_else(|| std::path::Path::new("."));

    let mut utterances = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let mut fields = line.splitn(3, '\t');
        let (Some(id), Some(wav), Some(reference)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return Err(format!("line {} of the manifest is not id/wav/reference", number + 1).into());
        };

        utterances.push(Utterance {
            id: id.to_string(),
            path: root.join(wav),
            reference: reference.to_string(),
        });
    }

    Ok(utterances)
}

fn parse(args: Vec<String>) -> Result<Options, String> {
    let mut options = Options {
        manifest: PathBuf::new(),
        models: PathBuf::from(
            std::env::var("VERSE_MODELS").unwrap_or_else(|_| DEFAULT_MODELS.to_string()),
        ),
        engine: DEFAULT_ENGINE.to_string(),
        limit: None,
        dump: None,
        itn: true,
        vad_threshold: verse_audio::DEFAULT_THRESHOLD,
    };

    let mut manifest: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--models" => options.models = PathBuf::from(take(&args, &mut i, "--models")?),
            "--engine" => options.engine = take(&args, &mut i, "--engine")?,
            "--limit" => {
                let raw = take(&args, &mut i, "--limit")?;
                options.limit = Some(raw.parse().map_err(|_| format!("--limit {raw} is not a number"))?);
            }
            "--dump" => options.dump = Some(PathBuf::from(take(&args, &mut i, "--dump")?)),
            "--no-itn" => options.itn = false,
            "--vad-threshold" => {
                let raw = take(&args, &mut i, "--vad-threshold")?;
                options.vad_threshold = raw
                    .parse()
                    .map_err(|_| format!("--vad-threshold {raw} is not a number"))?;
            }
            other if other.starts_with('-') => return Err(format!("unknown option '{other}'")),
            other => {
                if manifest.is_some() {
                    return Err(format!("unexpected extra argument '{other}'"));
                }
                manifest = Some(PathBuf::from(other));
            }
        }
        i += 1;
    }

    options.manifest = manifest.ok_or("usage: verse-bench <manifest.tsv> [options]")?;
    Ok(options)
}

fn take(args: &[String], i: &mut usize, name: &str) -> Result<String, String> {
    *i += 1;
    args.get(*i).cloned().ok_or_else(|| format!("{name} needs a value"))
}
