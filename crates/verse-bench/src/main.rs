//! Scores the recognizer against labelled datasets.
//!
//!     verse-bench <manifest.tsv>... [--models <dir>] [--engine <id>]
//!                                  [--limit N] [--no-itn]
//!                                  [--vad-threshold F] [--dump <file>]
//!
//! A manifest is `<id>\t<wav>\t<reference>`, as written by
//! `tools/asr-eval/extract.py`.
//!
//! **It runs the shipped pipeline**, not a copy of it. A benchmark that
//! transcribed files its own way would be measuring a program nobody uses —
//! and the boundary decisions in the pipeline are exactly what is under
//! examination here.
//!
//! One manifest prints the full account; several print a table. Domain
//! matters more than sample size here: an average over clean read speech and
//! a phone call describes neither.

mod cer;
mod punct;
mod report;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use verse_core::{CancelToken, EventBus, JobId};
use verse_pipeline::{Request, Transcriber};

use report::{Dataset, Scored};

const DEFAULT_ENGINE: &str = "sensevoice";
const DEFAULT_MODELS: &str = "models";
const WORST_SHOWN: usize = 20;

/// How many utteracks to read before deciding whether references carry
/// punctuation. The first few are enough; a dataset is not half punctuated.
const PUNCTUATION_PROBE: usize = 200;

struct Options {
    manifests: Vec<PathBuf>,
    models: PathBuf,
    engine: String,
    limit: Option<usize>,
    dump: Option<PathBuf>,
    itn: bool,
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

    let request = Request {
        input: PathBuf::new(),
        models_dir: options.models.clone(),
        engine: options.engine.clone(),
        vad: Request::vad_for(&options.models),
        inverse_text_normalization: options.itn,
        vad_threshold: options.vad_threshold,
    };

    eprintln!("loading {} ...", options.engine);
    let load_started = Instant::now();
    let mut transcriber = Transcriber::load(request)?;
    eprintln!("loaded in {:.1}s", load_started.elapsed().as_secs_f64());

    let mut datasets = Vec::new();
    for manifest in &options.manifests {
        datasets.push(score_dataset(&mut transcriber, manifest, &options)?);
    }

    if datasets.len() == 1 {
        report::detail(&datasets[0], WORST_SHOWN);
    } else {
        report::summary(&datasets);
    }

    if let Some(path) = &options.dump {
        if let Some(only) = datasets.first() {
            report::dump(path, &only.results)?;
            eprintln!("every pair written to {}", path.display());
        }
    }

    Ok(())
}

fn score_dataset(
    transcriber: &mut Transcriber,
    manifest: &Path,
    options: &Options,
) -> Result<Dataset, Box<dyn std::error::Error>> {
    let name = manifest
        .parent()
        .and_then(|dir| dir.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| manifest.display().to_string());

    let mut utterances = read_manifest(manifest)?;
    if let Some(limit) = options.limit {
        utterances.truncate(limit);
    }

    if utterances.is_empty() {
        return Err(format!("{} has no usable rows", manifest.display()).into());
    }

    // Whether punctuation is worth scoring is a property of the references,
    // not of the recogniser.
    let scored_punctuation = utterances
        .iter()
        .take(PUNCTUATION_PROBE)
        .any(|u| u.reference.contains(['，', '。', '？', '！']));

    // The bus is required by the pipeline but nothing here listens; results
    // come back through the return value.
    let bus = EventBus::new();
    let cancel = CancelToken::new();

    let mut results = Vec::with_capacity(utterances.len());
    let mut errors = 0usize;
    let mut characters = 0usize;
    let mut by_mark: BTreeMap<char, punct::MarkScore> = BTreeMap::new();
    let mut punctuation = punct::MarkScore::default();

    let started = Instant::now();

    for (index, utterance) in utterances.iter().enumerate() {
        transcriber.set_input(utterance.path.clone());

        let transcript = match transcriber.transcribe(JobId(index as u64 + 1), &bus, &cancel) {
            Ok(transcript) => transcript,
            Err(error) => {
                // A file that will not open is worth knowing about, but it is
                // not a recognition error and must not be folded into the rate.
                eprintln!("  {} failed: {}", utterance.id, error.message());
                continue;
            }
        };

        let hypothesis = transcript.to_text().replace('\n', "");
        let score = cer::score(&utterance.reference, &hypothesis);

        errors += score.errors;
        characters += score.reference_len;

        if scored_punctuation {
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
        }

        results.push(Scored {
            id: utterance.id.clone(),
            reference: utterance.reference.clone(),
            hypothesis,
            score,
        });

        if (index + 1) % 200 == 0 {
            eprint!("\r  {name}: {}/{} ...", index + 1, utterances.len());
        }
    }

    let elapsed_seconds = started.elapsed().as_secs_f64();
    eprintln!("\r  {name}: {} scored in {elapsed_seconds:.1}s", results.len());

    Ok(Dataset {
        name,
        results,
        errors,
        characters,
        by_mark,
        punctuation,
        scored_punctuation,
        elapsed_seconds,
    })
}

struct Utterance {
    id: String,
    path: PathBuf,
    reference: String,
}

fn read_manifest(path: &Path) -> Result<Vec<Utterance>, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)?;
    let root = path.parent().unwrap_or_else(|| Path::new("."));

    let mut utterances = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let mut fields = line.splitn(3, '\t');
        let (Some(id), Some(wav), Some(reference)) = (fields.next(), fields.next(), fields.next())
        else {
            return Err(
                format!("line {} of the manifest is not id/wav/reference", number + 1).into(),
            );
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
        manifests: Vec::new(),
        models: PathBuf::from(
            std::env::var("VERSE_MODELS").unwrap_or_else(|_| DEFAULT_MODELS.to_string()),
        ),
        engine: DEFAULT_ENGINE.to_string(),
        limit: None,
        dump: None,
        itn: true,
        vad_threshold: verse_audio::DEFAULT_THRESHOLD,
    };

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--models" => options.models = PathBuf::from(take(&args, &mut i, "--models")?),
            "--engine" => options.engine = take(&args, &mut i, "--engine")?,
            "--dump" => options.dump = Some(PathBuf::from(take(&args, &mut i, "--dump")?)),
            "--no-itn" => options.itn = false,
            "--limit" => {
                let raw = take(&args, &mut i, "--limit")?;
                options.limit = Some(
                    raw.parse()
                        .map_err(|_| format!("--limit {raw} is not a number"))?,
                );
            }
            "--vad-threshold" => {
                let raw = take(&args, &mut i, "--vad-threshold")?;
                options.vad_threshold = raw
                    .parse()
                    .map_err(|_| format!("--vad-threshold {raw} is not a number"))?;
            }
            other if other.starts_with('-') => return Err(format!("unknown option '{other}'")),
            other => options.manifests.push(PathBuf::from(other)),
        }
        i += 1;
    }

    if options.manifests.is_empty() {
        return Err("usage: verse-bench <manifest.tsv>... [options]".to_string());
    }
    if options.dump.is_some() && options.manifests.len() > 1 {
        return Err("--dump takes one manifest; it writes one file".to_string());
    }

    Ok(options)
}

fn take(args: &[String], i: &mut usize, name: &str) -> Result<String, String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| format!("{name} needs a value"))
}
