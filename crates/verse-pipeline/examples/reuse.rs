//! What keeping a model loaded is worth, measured rather than asserted.
//!
//! ```
//! cargo run -p verse-pipeline --example reuse -- --times 5
//! ```
//!
//! Two phases over the same clip. Phase A loads a model for every file, which
//! is what the window did until the keeper existed; phase B runs the same files
//! through one `ModelKeeper`. Both print per-file milliseconds and the number of
//! times a model was read from disk.
//!
//! **The load count is the instrument, and it is validated in the same run.**
//! With `--times 1` both phases must report exactly one load — if they do not,
//! the counter is broken and the timings beside it mean nothing. This project
//! has already been misled once by an instrument that had no known-good
//! reading.
//!
//! The result cache is off in both phases, deliberately. A cache hit in phase B
//! would measure the cache, which is a different feature and a different
//! measurement.

use std::path::{Path, PathBuf};
use std::time::Instant;

use verse_core::{CancelToken, EventBus, JobId};
use verse_pipeline::{CachePolicy, GuardSettings, ModelKeeper, Request, Transcriber};

fn main() {
    let times = number(&arguments(), "--times").unwrap_or(5).max(1);
    let input = path(&arguments(), "--file").unwrap_or_else(default_input);

    if !input.is_file() {
        eprintln!("no audio at {}", input.display());
        eprintln!("pass --file <path>; the default is models/sensevoice/zh.wav");
        std::process::exit(2);
    }

    println!("input: {}", input.display());
    println!("files: {times}");
    println!();

    let (loaded_each, loads_a) = load_each_time(&input, times);
    report("A  load per file (what the window did)", &loaded_each, loads_a, times);

    let (kept, loads_b) = keep_one(&input, times);
    report("B  one keeper         (what it does now)", &kept, loads_b, times);

    println!();
    println!(
        "loads: A={loads_a} B={loads_b}   totals: A={} ms  B={} ms",
        loaded_each.iter().sum::<u128>(),
        kept.iter().sum::<u128>()
    );

    // The instrument's own check, printed rather than left to the reader.
    if times == 1 {
        if loads_a == 1 && loads_b == 1 {
            println!("instrument: both phases loaded once, as they must at --times 1");
        } else {
            println!("instrument: BROKEN — at --times 1 both must load once");
        }
    }
}

/// Phase A: a fresh model for every file.
fn load_each_time(input: &Path, times: usize) -> (Vec<u128>, u64) {
    let bus = EventBus::new();
    let cancel = CancelToken::new();
    let mut per_file = Vec::with_capacity(times);
    let mut loads = 0;

    for index in 0..times {
        let started = Instant::now();
        let transcriber = Transcriber::load(request_for(input));
        loads += 1;

        match transcriber {
            Ok(mut transcriber) => {
                let _ = transcriber.transcribe(JobId(index as u64 + 1), &bus, &cancel);
            }
            Err(error) => {
                eprintln!("could not load a model: {error}");
                std::process::exit(3);
            }
        }

        per_file.push(started.elapsed().as_millis());
    }

    (per_file, loads)
}

/// Phase B: one model, kept.
fn keep_one(input: &Path, times: usize) -> (Vec<u128>, u64) {
    let bus = EventBus::new();
    // No idle timeout: the point is to hold the model, not to watch it go.
    let keeper = ModelKeeper::with_timeout(bus, None);
    let cancel = CancelToken::new();
    let mut per_file = Vec::with_capacity(times);

    for index in 0..times {
        let started = Instant::now();
        match keeper.transcribe(request_for(input), JobId(index as u64 + 1), &cancel) {
            Ok(_) => {}
            Err(error) => eprintln!("file {index} failed: {error}"),
        }
        per_file.push(started.elapsed().as_millis());
    }

    (per_file, keeper.loads())
}

fn request_for(input: &Path) -> Request {
    let models_dir = workspace().join("models");

    Request {
        input: input.to_path_buf(),
        vad_model: Request::vad_for(&models_dir),
        models_dir,
        engine: "sensevoice".to_string(),
        inverse_text_normalization: true,
        vad: verse_audio::VadSettings::default(),
        max_output_tokens: None,
        guard: GuardSettings::default(),
        hotwords: None,
        threads: None,
        // Off, so phase B cannot be measured as the cache.
        cache: CachePolicy::Disabled,
    }
}

fn report(label: &str, per_file: &[u128], loads: u64, times: usize) {
    let total: u128 = per_file.iter().sum();
    println!("{label}");
    println!(
        "   per file: {}  ms",
        per_file
            .iter()
            .map(|ms| ms.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "   {loads} load{}, {total} ms total, {} ms average",
        if loads == 1 { "" } else { "s" },
        total / times.max(1) as u128
    );
    println!();
}

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .to_path_buf()
}

fn default_input() -> PathBuf {
    workspace().join("models").join("sensevoice").join("zh.wav")
}

fn arguments() -> Vec<String> {
    std::env::args().skip(1).collect()
}

fn number(args: &[String], flag: &str) -> Option<usize> {
    let index = args.iter().position(|arg| arg == flag)?;
    args.get(index + 1)?.parse().ok()
}

fn path(args: &[String], flag: &str) -> Option<PathBuf> {
    let index = args.iter().position(|arg| arg == flag)?;
    Some(PathBuf::from(args.get(index + 1)?))
}
