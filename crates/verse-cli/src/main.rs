//! Verse command line.
//!
//! Phase 1 entry point: fetch models, transcribe files, offline.
//!
//! Argument parsing is hand-rolled. The surface is small and stable, and the
//! project's whole point is a small footprint — pulling in an argument parser
//! for a handful of flags would be out of proportion.

mod report;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use verse_core::{CancelToken, ErrorKind, EventBus, ExportFormat, HardwareProfile, JobId};
use verse_model::{Catalog, DownloadState, Downloader};
use verse_pipeline::{GuardSettings, Request, Transcriber};

const DEFAULT_ENGINE: &str = "sensevoice";
const DEFAULT_MODELS_DIR: &str = "models";

/// How a command failed, and what the shell should be told about it.
///
/// A program driving this from a script should not have to read the message to
/// learn what went wrong, so the codes separate the failures that call for
/// *different actions* — fetch a model, pick another file, retry later — from
/// the ones that differ only in wording.
#[derive(Debug)]
struct Failure {
    code: u8,
    message: String,
}

/// Exit codes, in one place because they are a published contract.
///
/// `verse-bench` already uses 2 for a usage error; the rest continue from
/// there.
fn exit_code(kind: ErrorKind) -> u8 {
    match kind {
        // The input itself is the problem: unreadable, or not decodable.
        ErrorKind::Io | ErrorKind::Decode => 3,
        // Nothing can be recognised until a model is installed. `verse model
        // fetch` is the action, which is why this is worth its own code.
        ErrorKind::Model | ErrorKind::Registry => 4,
        ErrorKind::Engine => 5,
        ErrorKind::Network => 6,
        ErrorKind::Cancelled => 7,
        ErrorKind::Sink => 8,
        // A bug. Should not be reachable through normal use.
        ErrorKind::Internal => 1,
    }
}

impl Failure {
    /// The command line was wrong. Nothing was attempted.
    fn usage(message: impl Into<String>) -> Self {
        Self {
            code: 2,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: message.into(),
        }
    }
}

impl From<verse_core::Error> for Failure {
    fn from(error: verse_core::Error) -> Self {
        Self {
            code: exit_code(error.kind()),
            message: error.message().to_string(),
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let result = match args.first().map(String::as_str) {
        Some("transcribe") => {
            // Asking a command about itself is the first thing anyone does,
            // and an agent exploring the surface does it more than anyone.
            if args[1..].iter().any(|a| a == "-h" || a == "--help") {
                print!("{}", transcribe_usage());
                Ok(())
            } else {
                TranscribeOptions::parse(&args[1..]).and_then(|options| transcribe(&options))
            }
        }
        Some("model") => model_command(&args[1..]),
        Some("--version") | Some("-V") => {
            println!("verse {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help") | Some("-h") | None => {
            print_usage();
            Ok(())
        }
        Some(other) => Err(Failure::usage(format!(
            "unknown command '{other}'\n\nrun 'verse --help'"
        ))),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("error: {}", failure.message);
            ExitCode::from(failure.code)
        }
    }
}

fn print_usage() {
    println!("verse {}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("usage: verse <command> [options]");
    println!();
    println!("commands:");
    println!("  transcribe <file>     Transcribe an audio or video file");
    println!("  model list            Show known models and whether they are present");
    println!("  model fetch <id>      Download a model");
    println!();
    println!("  --help, -h            Show this message");
    println!("  --version, -V         Show the version");
}

// ---------------------------------------------------------------- transcribe

struct TranscribeOptions {
    inputs: Vec<PathBuf>,
    output: Option<PathBuf>,
    format: Option<ExportFormat>,
    engine: String,
    models_dir: PathBuf,
    vad: Option<PathBuf>,
    /// Stop at the first file that fails rather than finishing the batch.
    fail_fast: bool,
    /// Emit one JSON document on stdout instead of prose on stderr.
    json: bool,
    /// Domain vocabulary for an engine that can take one.
    hotwords: Option<String>,
    /// How many files to recognise at once. One engine per worker, so this
    /// costs a model's worth of memory each.
    jobs: usize,
}

impl TranscribeOptions {
    fn parse(args: &[String]) -> Result<Self, Failure> {
        let mut options = Self {
            inputs: Vec::new(),
            output: None,
            format: None,
            engine: DEFAULT_ENGINE.to_string(),
            models_dir: PathBuf::from(DEFAULT_MODELS_DIR),
            vad: None,
            fail_fast: false,
            json: false,
            hotwords: None,
            jobs: 1,
        };

        let mut i = 0;
        while i < args.len() {
            let arg = args[i].clone();
            match arg.as_str() {
                "-o" | "--output" => {
                    options.output = Some(PathBuf::from(take_value(args, &mut i, "--output")?))
                }
                "--format" => {
                    let raw = take_value(args, &mut i, "--format")?;
                    options.format = Some(
                        ExportFormat::from_extension(&raw)
                            .ok_or_else(|| Failure::usage(format!("unknown format '{raw}'")))?,
                    );
                }
                "--engine" => options.engine = take_value(args, &mut i, "--engine")?,
                "--models" => {
                    options.models_dir = PathBuf::from(take_value(args, &mut i, "--models")?)
                }
                "--vad" => options.vad = Some(PathBuf::from(take_value(args, &mut i, "--vad")?)),
                "--fail-fast" => options.fail_fast = true,
                "--json" => options.json = true,
                "-j" | "--jobs" => {
                    let raw = take_value(args, &mut i, "--jobs")?;
                    let jobs: usize = raw
                        .parse()
                        .map_err(|_| Failure::usage(format!("--jobs {raw} is not a number")))?;
                    if jobs == 0 {
                        return Err(Failure::usage("--jobs needs at least 1".to_string()));
                    }
                    options.jobs = jobs;
                }
                "--hotwords" => {
                    options.hotwords = Some(take_value(args, &mut i, "--hotwords")?)
                }
                // A lexicon long enough to be worth a file. Read verbatim and
                // sent down the same path: the delimiter grammar belongs to
                // sherpa-onnx, and inventing a second one here would be a
                // second place to be wrong.
                "--hotwords-file" => {
                    let path = PathBuf::from(take_value(args, &mut i, "--hotwords-file")?);
                    options.hotwords = Some(std::fs::read_to_string(&path).map_err(|e| {
                        Failure::usage(format!("could not read {}: {e}", path.display()))
                    })?);
                }
                other if other.starts_with('-') => {
                    return Err(Failure::usage(format!(
                        "unknown option '{other}'\n\n{}",
                        transcribe_usage()
                    )));
                }
                // Any number of positionals: a file, or a directory to expand.
                other => options.inputs.push(PathBuf::from(other)),
            }
            i += 1;
        }

        if options.inputs.is_empty() {
            return Err(Failure::usage(format!(
                "no input given\n\n{}",
                transcribe_usage()
            )));
        }

        Ok(options)
    }
}

/// Containers ffmpeg can pull an audio track out of.
///
/// Consulted only when deciding what a **directory** expands to. A file named
/// explicitly on the command line is always attempted, whatever it is called —
/// refusing it by extension would turn a decodable file into a usage error,
/// and the decoder is the only thing that actually knows.
const AUDIO_EXTENSIONS: &[&str] = &[
    "wav", "mp3", "mp2", "m4a", "aac", "flac", "ogg", "opus", "wma", "amr", "aiff", "aif", "caf",
    "mp4", "mkv", "mov", "webm", "avi", "ts",
];

/// Turn what was typed into the list of files to transcribe.
fn expand_inputs(positionals: &[PathBuf]) -> Result<Vec<PathBuf>, Failure> {
    let mut files = Vec::new();

    for path in positionals {
        if path.is_dir() {
            collect_audio(path, &mut files).map_err(Failure::from)?;
        } else {
            files.push(path.clone());
        }
    }

    // Sorted and deduplicated so the same command produces the same order and
    // the same report. An agent that re-runs a batch should not get a
    // different arrangement of the same work.
    files.sort();
    files.dedup();

    if files.is_empty() {
        return Err(Failure::usage(
            "no audio files found in the directories given".to_string(),
        ));
    }

    Ok(files)
}

fn collect_audio(dir: &Path, out: &mut Vec<PathBuf>) -> verse_core::Result<()> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        verse_core::Error::new(
            ErrorKind::Io,
            format!("could not read {}: {e}", dir.display()),
        )
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| verse_core::Error::new(ErrorKind::Io, format!("{e}")))?;
        // `file_type` does not follow symlinks, so a link pointing back up the
        // tree cannot send this into a loop.
        let kind = entry
            .file_type()
            .map_err(|e| verse_core::Error::new(ErrorKind::Io, format!("{e}")))?;
        let path = entry.path();

        if kind.is_dir() {
            collect_audio(&path, out)?;
        } else if kind.is_file() && has_audio_extension(&path) {
            out.push(path);
        }
    }

    Ok(())
}

fn has_audio_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.as_str()))
}

/// Where each input's transcript goes.
///
/// With one input the rules are exactly what they were before this command
/// took several, so nothing that used to work behaves differently. More than
/// one input forces a decision that did not exist: `-o` can only name a
/// directory, because one file cannot hold several transcripts.
fn resolve_outputs(
    inputs: &[PathBuf],
    output: Option<&Path>,
    format: ExportFormat,
) -> Result<Vec<PathBuf>, Failure> {
    let stdout = Path::new("-");

    if let [only] = inputs {
        return Ok(vec![match output {
            Some(path) => path.to_path_buf(),
            None => only.with_extension(format.extension()),
        }]);
    }

    if let Some(path) = output {
        if path == stdout {
            return Err(Failure::usage(format!(
                "'-o -' writes one transcript to stdout and cannot hold {}",
                inputs.len()
            )));
        }
        // Either it is a file, or it is named like one. `-o out.srt` with
        // several inputs is a mistake worth reporting rather than a directory
        // worth creating — it would silently become `out.srt/a.srt`.
        let named_like_a_file = path.is_file()
            || path
                .extension()
                .and_then(|e| e.to_str())
                .and_then(ExportFormat::from_extension)
                .is_some();

        if named_like_a_file {
            return Err(Failure::usage(format!(
                "'-o {}' names a file, but there are {} inputs; name a directory instead",
                path.display(),
                inputs.len()
            )));
        }
    }

    let planned: Vec<PathBuf> = inputs
        .iter()
        .map(|input| match output {
            Some(dir) => dir
                .join(input.file_stem().unwrap_or_default())
                .with_extension(format.extension()),
            None => input.with_extension(format.extension()),
        })
        .collect();

    // Two inputs differing only by extension — `a.wav` and `a.mp3` — would
    // write to the same path. Caught here, before anything is recognised,
    // because the alternative is discovering it after one transcript has
    // already overwritten the other.
    for (i, path) in planned.iter().enumerate() {
        if let Some(j) = planned[..i].iter().position(|earlier| earlier == path) {
            return Err(Failure::usage(format!(
                "'{}' and '{}' would both be written to '{}'; rename one or use -o",
                inputs[j].display(),
                inputs[i].display(),
                path.display()
            )));
        }
    }

    Ok(planned)
}

fn transcribe_usage() -> String {
    let mut s = String::from("usage: verse transcribe <file|dir>... [options]\n\noptions:\n");
    s.push_str("  -o, --output <path>    Output file, or a directory for several inputs\n");
    s.push_str("                         (default: each input keeps its name, extension swapped)\n");
    s.push_str("      --format <srt|txt> Output format (default: srt)\n");
    s.push_str(&format!(
        "      --engine <id>      Engine (default: {DEFAULT_ENGINE})\n"
    ));
    s.push_str(&format!(
        "      --models <dir>     Model root (default: {DEFAULT_MODELS_DIR})\n"
    ));
    s.push_str("      --vad <file>       VAD model (default: <models>/silero-vad/silero_vad.onnx)\n");
    s.push_str("      --hotwords <terms> Domain vocabulary, for engines that take one\n");
    s.push_str("      --hotwords-file <f>\n");
    s.push_str("                         The same, read from a file\n");
    s.push_str("  -j, --jobs <n>         Files to recognise at once (default 1). Each worker\n");
    s.push_str("                         loads its own model: about 250 MB for SenseVoice,\n");
    s.push_str("                         1 GB for Qwen3.\n");
    s.push_str("      --json             One JSON document on stdout\n");
    s.push_str("      --fail-fast        Stop at the first file that fails\n");
    s.push_str("\nA directory argument is expanded to the audio files beneath it, sorted.\n");
    s
}

fn transcribe(options: &TranscribeOptions) -> Result<(), Failure> {
    // Reported once, and only when something is being worked around.
    let hardware = HardwareProfile::probe();
    if let Some(notice) = hardware.tier().notice() {
        eprintln!("{notice}");
    }

    let inputs = expand_inputs(&options.inputs)?;
    let format = options.format.unwrap_or(ExportFormat::Srt);
    let outputs = resolve_outputs(&inputs, options.output.as_deref(), format)?;

    // One directory holding several transcripts has to exist first; the
    // single-input case writes beside a file that is already there.
    if let (Some(dir), true) = (&options.output, inputs.len() > 1) {
        std::fs::create_dir_all(dir).map_err(|e| {
            Failure {
                code: exit_code(ErrorKind::Sink),
                message: format!("could not create {}: {e}", dir.display()),
            }
        })?;
    }

    // The chain itself lives in `verse-pipeline`, shared with the window and
    // the benchmark. This command owns only the arguments and the output files.
    let request = Request {
        input: inputs[0].clone(),
        models_dir: options.models_dir.clone(),
        engine: options.engine.clone(),
        vad_model: options
            .vad
            .clone()
            .unwrap_or_else(|| Request::vad_for(&options.models_dir)),
        // What a person would write: numbers normalised, punctuation kept.
        inverse_text_normalization: true,
        vad: verse_audio::VadSettings::default(),
        max_output_tokens: None,
        guard: GuardSettings::default(),
        hotwords: options.hotwords.clone(),
        threads: None,
    };

    // **Loaded once for the whole batch.** This is the reason `Transcriber`
    // exists as a type at all: the model is 228 MB, and loading it per file
    // costs more than recognising a short utterance does.
    // Asked before anything is loaded, so the warning arrives at once rather
    // than after a 228 MB wait. A lexicon handed to an engine that cannot use
    // one must not pass in silence: the result would be indistinguishable
    // from a lexicon that simply did not help.
    let mut warnings = Vec::new();
    let hotwords_applied = match &options.hotwords {
        Some(_) if !verse_pipeline::engine_accepts_hotwords(&options.engine) => {
            let warning = format!(
                "engine '{}' cannot use a domain vocabulary; --hotwords was ignored",
                options.engine
            );
            eprintln!("warning: {warning}");
            warnings.push(warning);
            None
        }
        given => given.clone(),
    };

    let multiple = inputs.len() > 1;
    // A live progress line is worth having while waiting and worth nothing in
    // a log. Redirected stderr keeps the result lines and loses the carriage
    // returns that would otherwise litter it.
    let live = std::io::stderr().is_terminal();

    let started = Instant::now();
    // Nothing subscribes; transcripts come back through the return value.
    let bus = EventBus::new();
    let cancel = CancelToken::new();

    let mut results = if options.jobs > 1 {
        // Loading several models takes as long as loading one does per
        // worker, and saying nothing through it looks like a hang.
        let workers = options.jobs.min(inputs.len()).max(1);
        eprintln!("loading {} ...", options.engine);
        run_parallel(
            &inputs, &outputs, format, &request, &bus, &cancel, options, &hardware, workers,
        )
    } else {
        // **Loaded once for the whole batch.** This is why `Transcriber`
        // exists as a type: the model is 228 MB, and loading it per file costs
        // more than recognising a short utterance does.
        eprintln!("loading {} ...", options.engine);
        let mut transcriber = Transcriber::load(request)?;
        run_sequential(
            &inputs,
            &outputs,
            format,
            &mut transcriber,
            &bus,
            &cancel,
            options,
            multiple && live && !options.json,
        )
    };

    // Input order, whatever order the work finished in, so the same command
    // always produces the same document.
    let order: Vec<String> = inputs.iter().map(|p| p.display().to_string()).collect();
    results.sort_by_key(|result| {
        order
            .iter()
            .position(|name| *name == result.input)
            .unwrap_or(usize::MAX)
    });

    let failed = results.iter().filter(|result| !result.ok).count();
    let code = exit_code(first_failure_kind(&results));

    if options.json {
        report::Report::new(
            options.engine.clone(),
            options.models_dir.display().to_string(),
            hotwords_applied,
            warnings,
            results,
            started.elapsed().as_millis() as u64,
        )
        .print();
    }

    if failed > 0 {
        return Err(Failure {
            code,
            message: format!("{} of {} file(s) failed", failed, inputs.len()),
        });
    }

    Ok(())
}

/// The kind of the first file that failed, which becomes the exit code.
///
/// Read back out of the report rather than tracked alongside it, so the two
/// can never disagree about what happened.
fn first_failure_kind(results: &[report::FileResult]) -> ErrorKind {
    results
        .iter()
        .filter_map(|result| result.error.as_ref())
        .next()
        .map(|error| kind_from_name(error.kind))
        .unwrap_or(ErrorKind::Internal)
}

/// The inverse of `report::kind_name`.
fn kind_from_name(name: &str) -> ErrorKind {
    match name {
        "io" => ErrorKind::Io,
        "network" => ErrorKind::Network,
        "decode" => ErrorKind::Decode,
        "model" => ErrorKind::Model,
        "engine" => ErrorKind::Engine,
        "sink" => ErrorKind::Sink,
        "registry" => ErrorKind::Registry,
        "cancelled" => ErrorKind::Cancelled,
        _ => ErrorKind::Internal,
    }
}

/// Recognise one file and say what happened, whatever that was.
///
/// Extracted so the sequential and parallel paths differ only in who calls
/// this and in what order the answers come back — never in what a file's
/// outcome means.
fn run_one(
    transcriber: &mut Transcriber,
    job: JobId,
    input: &Path,
    output: &Path,
    format: ExportFormat,
    bus: &EventBus,
    cancel: &CancelToken,
) -> report::FileResult {
    let started = Instant::now();
    transcriber.set_input(input.to_path_buf());

    let transcription = match transcriber.transcribe(job, bus, cancel) {
        Ok(transcription) => transcription,
        Err(error) => {
            let kind = error.kind();
            let failure = Failure::from(error);
            // Kept on stderr even under `--json`: the report says what failed,
            // but only the message says why.
            eprintln!("error: {}: {}", input.display(), failure.message);
            return report::FileResult::failed(
                input,
                format.extension(),
                started.elapsed().as_millis() as u64,
                kind,
                failure.message,
            );
        }
    };

    let rendered = format.render(&transcription.transcript);
    if let Err(failure) = write_output(output, &rendered) {
        eprintln!("error: {}", failure.message);
        return report::FileResult::failed(
            input,
            format.extension(),
            started.elapsed().as_millis() as u64,
            ErrorKind::Sink,
            failure.message,
        );
    }

    report::FileResult::transcribed(
        input,
        Some(output),
        format.extension(),
        started.elapsed().as_millis() as u64,
        &transcription,
    )
}

/// One file at a time, with a live line naming the file in progress.
#[allow(clippy::too_many_arguments)]
fn run_sequential(
    inputs: &[PathBuf],
    outputs: &[PathBuf],
    format: ExportFormat,
    transcriber: &mut Transcriber,
    bus: &EventBus,
    cancel: &CancelToken,
    options: &TranscribeOptions,
    live: bool,
) -> Vec<report::FileResult> {
    let multiple = inputs.len() > 1;
    let mut results = Vec::with_capacity(inputs.len());

    for (index, (input, output)) in inputs.iter().zip(outputs).enumerate() {
        if multiple && live {
            eprint!("[{}/{}] {} ...", index + 1, inputs.len(), short_name(input));
        }

        let result = run_one(
            transcriber,
            JobId(index as u64 + 1),
            input,
            output,
            format,
            bus,
            cancel,
        );

        if !options.json {
            if result.ok {
                if multiple {
                    if live {
                        clear_line();
                    }
                    eprintln!(
                        "[{}/{}] {} -> {} ({}, {})",
                        index + 1,
                        inputs.len(),
                        short_name(input),
                        short_name(output),
                        count(result.segment_count, "segment"),
                        describe(&result),
                    );
                } else {
                    // How much of the file's sound reached the recogniser.
                    // This is the one number that separates a transcript that
                    // is short because the recording was short from one that
                    // is short because audio was dropped.
                    eprintln!("{}", describe(&result));
                    eprintln!("{} segments -> {}", result.segment_count, output.display());
                }
            } else if multiple && live {
                clear_line();
            }
        }

        let stop = !result.ok && options.fail_fast;
        results.push(result);
        if stop {
            break;
        }
    }

    results
}

/// Several files at once, one engine per worker.
///
/// The cost of this mode is the models: each worker loads its own, 228 MB for
/// SenseVoice and 982 MB for Qwen3. The engine's thread count is already
/// cores − 1, so the budget is divided between the workers rather than each
/// asking for the whole machine.
#[allow(clippy::too_many_arguments)]
fn run_parallel(
    inputs: &[PathBuf],
    outputs: &[PathBuf],
    format: ExportFormat,
    request: &Request,
    bus: &EventBus,
    cancel: &CancelToken,
    options: &TranscribeOptions,
    hardware: &HardwareProfile,
    workers: usize,
) -> Vec<report::FileResult> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    let per_worker_threads = (hardware.engine_threads() / workers).max(1);

    let next = AtomicUsize::new(0);
    let collected: Mutex<Vec<Option<report::FileResult>>> =
        Mutex::new((0..inputs.len()).map(|_| None).collect());
    let load_failure: Mutex<Option<Failure>> = Mutex::new(None);
    // Held while a line is printed, so two workers cannot interleave halves
    // of two different lines.
    let console = Mutex::new(());

    std::thread::scope(|scope| {
        for _ in 0..workers {
            let mut worker_request = request.clone();
            worker_request.threads = Some(per_worker_threads);

            scope.spawn(|| {
                let mut transcriber = match Transcriber::load(worker_request) {
                    Ok(transcriber) => transcriber,
                    Err(error) => {
                        // Every worker fails the same way, so the first to
                        // notice is the one that gets to say so.
                        let mut slot = load_failure.lock().expect("poisoned");
                        if slot.is_none() {
                            *slot = Some(Failure::from(error));
                        }
                        return;
                    }
                };

                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= inputs.len() {
                        break;
                    }

                    let result = run_one(
                        &mut transcriber,
                        JobId(index as u64 + 1),
                        &inputs[index],
                        &outputs[index],
                        format,
                        bus,
                        cancel,
                    );

                    if !options.json && result.ok {
                        let _guard = console.lock().expect("poisoned");
                        eprintln!(
                            "[{}/{}] {} -> {} ({}, {})",
                            index + 1,
                            inputs.len(),
                            short_name(&inputs[index]),
                            short_name(&outputs[index]),
                            count(result.segment_count, "segment"),
                            describe(&result),
                        );
                    }

                    collected.lock().expect("poisoned")[index] = Some(result);
                }
            });
        }
    });

    let failed_to_load = load_failure.into_inner().expect("poisoned");
    let results: Vec<report::FileResult> = collected
        .into_inner()
        .expect("poisoned")
        .into_iter()
        .flatten()
        .collect();

    // A load failure means nothing ran at all. That is a property of the
    // command, not of any one file, so it is reported as such rather than
    // as a hundred identical per-file failures.
    if let (Some(failure), true) = (failed_to_load, results.is_empty()) {
        eprintln!("error: {}", failure.message);
        std::process::exit(i32::from(failure.code));
    }

    results
}


// --------------------------------------------------------------------- model

/// What the coverage measurement says about one file, in a few words.
///
/// The whole point of carrying these numbers out of the pipeline is that a
/// short transcript and a truncated one look alike; this is where the
/// difference gets said out loud.
fn describe(result: &report::FileResult) -> String {
    let kept = match result.coverage {
        Some(ratio) => format!("{:.0}% kept", ratio * 100.0),
        None => "no sound to judge".to_string(),
    };

    if result.recovered {
        format!("{kept}, detector lost it and the file was re-read")
    } else {
        kept
    }
}

/// Write one transcript, or print it when the output is `-`.
fn write_output(path: &Path, rendered: &str) -> Result<(), Failure> {
    if path == Path::new("-") {
        print!("{rendered}");
        return Ok(());
    }

    std::fs::write(path, rendered).map_err(|e| Failure {
        code: exit_code(ErrorKind::Sink),
        message: format!("could not write {}: {e}", path.display()),
    })
}

/// Bring the cursor back to the start of the line, and blank it.
///
/// Cleared rather than merely returned to: a short result printed over a long
/// file name leaves the tail of the name behind it.
fn clear_line() {
    eprint!("\r{:79}\r", "");
}

/// Just the file name, for a line that already has enough on it.
fn short_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn model_command(args: &[String]) -> Result<(), Failure> {
    match args.first().map(String::as_str) {
        Some("list") => model_list(&models_dir_from(args), args.iter().any(|a| a == "--json")),
        Some("fetch") => {
            let id = args
                .get(1)
                .filter(|a| !a.starts_with('-'))
                .ok_or_else(|| Failure::usage("usage: verse model fetch <id> [--models <dir>]"))?;
            model_fetch(id, &models_dir_from(args))
        }
        _ => Err(Failure::usage(
            "usage: verse model list | verse model fetch <id>",
        )),
    }
}

/// Pull `--models <dir>` out of a model subcommand's arguments.
fn models_dir_from(args: &[String]) -> PathBuf {
    args.iter()
        .position(|a| a == "--models")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_MODELS_DIR))
}

/// Where a catalogue override would be looked for.
fn catalog_path(models_dir: &Path) -> PathBuf {
    models_dir.join("catalog.json")
}

fn load_catalog(models_dir: &Path) -> Result<Catalog, Failure> {
    Catalog::load_or_embedded(&catalog_path(models_dir)).map_err(Failure::from)
}

fn model_list(models_dir: &Path, json: bool) -> Result<(), Failure> {
    let catalog = load_catalog(models_dir)?;

    if json {
        let models = catalog
            .models
            .iter()
            .map(|spec| report::ModelEntry {
                id: spec.id.clone(),
                display_name: spec.display_name.clone(),
                present: Downloader::is_present(spec, models_dir),
                directory: Downloader::directory_for(spec, models_dir)
                    .display()
                    .to_string(),
                mirrors: spec.mirrors.iter().map(|m| m.name.clone()).collect(),
            })
            .collect();

        report::print_json(&report::ModelList {
            version: report::VERSION,
            models_dir: models_dir.display().to_string(),
            models,
        });
        return Ok(());
    }

    println!("models directory: {}", models_dir.display());
    println!("catalogue: {}", catalog_path(models_dir).display());
    println!("           (edit it to change mirrors; falls back to the built-in copy)");
    println!();

    for spec in &catalog.models {
        let mark = if Downloader::is_present(spec, models_dir) {
            "present"
        } else {
            "missing"
        };
        println!("  {:<12} {:<8} {}", spec.id, mark, spec.display_name);

        if !Downloader::is_present(spec, models_dir) {
            for mirror in &spec.mirrors {
                println!("      via {}", mirror.name);
            }
        }
    }

    Ok(())
}

fn model_fetch(id: &str, models_dir: &Path) -> Result<(), Failure> {
    let catalog = load_catalog(models_dir)?;

    let spec = catalog
        .find(id)
        .ok_or_else(|| {
            Failure::usage(format!("unknown model '{id}'; run 'verse model list'"))
        })?;

    if Downloader::is_present(spec, models_dir) {
        println!("{} is already present", spec.display_name);
        return Ok(());
    }

    println!("fetching {}", spec.display_name);

    let downloader = Downloader::new();
    // No cancellation source yet; a later UI will drive this.
    let cancel = CancelToken::new();

    let state = downloader
        .fetch(spec, models_dir, report_progress, &cancel)
        .map_err(Failure::from)?;

    match state {
        DownloadState::Ready => {
            let dir = Downloader::directory_for(spec, models_dir);
            println!("ready: {}", dir.display());
            Ok(())
        }
        DownloadState::Idle => {
            println!("cancelled");
            Ok(())
        }
        DownloadState::Failed { reason } => Err(Failure::internal(reason)),
        other => Err(Failure::internal(format!("unexpected end state: {other:?}"))),
    }
}

/// Overwrite one line as the download advances.
fn report_progress(state: &DownloadState) {
    const CLEAR: &str = "\r                                        ";
    const MB: u64 = 1_000_000;

    match state {
        DownloadState::Fetching {
            mirror,
            file,
            received,
            total,
        } => match total {
            Some(total) if *total > 0 => eprint!(
                "{CLEAR}\r  {file} from {mirror}: {} / {} MB",
                received / MB,
                total / MB
            ),
            _ => eprint!("{CLEAR}\r  {file} from {mirror}: {} MB", received / MB),
        },
        DownloadState::Verifying => eprint!("{CLEAR}\r  verifying"),
        DownloadState::Ready => eprintln!("{CLEAR}\r  done"),
        DownloadState::Failed { reason } => eprintln!("{CLEAR}\r  failed: {reason}"),
        DownloadState::Idle => {}
    }
}

/// Consume the value that follows a flag.
fn take_value(args: &[String], i: &mut usize, name: &str) -> Result<String, Failure> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| Failure::usage(format!("{name} needs a value")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("verse-cli-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, b"x").expect("write");
    }

    #[test]
    fn a_named_file_is_taken_as_given_whatever_it_is_called() {
        // Only directory expansion filters by extension. Naming a file is an
        // instruction, and the decoder is the only thing that can decide
        // whether it is readable — refusing `.xyz` here would turn a decodable
        // recording into a usage error.
        let dir = scratch("explicit");
        let odd = dir.join("recording.xyz");
        touch(&odd);

        assert_eq!(expand_inputs(std::slice::from_ref(&odd)).unwrap(), vec![odd]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_expands_to_the_audio_beneath_it_sorted() {
        let dir = scratch("expand");
        for name in ["b.wav", "a.wav", "notes.txt"] {
            touch(&dir.join(name));
        }
        touch(&dir.join("nested/c.mp3"));
        touch(&dir.join("nested/readme.md"));

        let found = expand_inputs(std::slice::from_ref(&dir)).unwrap();
        let names: Vec<String> = found.iter().map(|p| short_name(p)).collect();

        // Sorted, so the same command always produces the same order.
        assert_eq!(names, vec!["a.wav", "b.wav", "c.mp3"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_with_nothing_to_read_is_a_usage_error() {
        let dir = scratch("empty");
        touch(&dir.join("notes.txt"));

        let failure = expand_inputs(std::slice::from_ref(&dir)).expect_err("should refuse");
        assert_eq!(failure.code, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_input_keeps_the_output_rules_it_had_alone() {
        let inputs = vec![PathBuf::from("/audio/a.m4a")];

        assert_eq!(
            resolve_outputs(&inputs, None, ExportFormat::Srt).unwrap(),
            vec![PathBuf::from("/audio/a.srt")]
        );
        assert_eq!(
            resolve_outputs(&inputs, Some(Path::new("/tmp/out.txt")), ExportFormat::Srt).unwrap(),
            vec![PathBuf::from("/tmp/out.txt")]
        );
    }

    #[test]
    fn several_inputs_write_into_the_directory_named() {
        let inputs = vec![PathBuf::from("/audio/a.wav"), PathBuf::from("/audio/b.wav")];

        assert_eq!(
            resolve_outputs(&inputs, Some(Path::new("/out")), ExportFormat::Srt).unwrap(),
            vec![PathBuf::from("/out/a.srt"), PathBuf::from("/out/b.srt")]
        );
    }

    #[test]
    fn several_inputs_with_no_output_go_beside_their_own_files() {
        let inputs = vec![PathBuf::from("/audio/a.wav"), PathBuf::from("/other/b.wav")];

        assert_eq!(
            resolve_outputs(&inputs, None, ExportFormat::Srt).unwrap(),
            vec![PathBuf::from("/audio/a.srt"), PathBuf::from("/other/b.srt")]
        );
    }

    #[test]
    fn two_inputs_that_would_share_an_output_are_refused() {
        // `a.wav` and `a.mp3` both want `a.srt`. Refused rather than
        // auto-suffixed: an unpredictable name is worse for whoever reads the
        // report than being told to rename one.
        let inputs = vec![PathBuf::from("/audio/a.wav"), PathBuf::from("/audio/a.mp3")];

        let failure = resolve_outputs(&inputs, None, ExportFormat::Srt).expect_err("should refuse");
        assert_eq!(failure.code, 2);
        assert!(failure.message.contains("a.srt"), "got: {}", failure.message);
    }

    #[test]
    fn one_output_file_cannot_hold_several_transcripts() {
        let inputs = vec![PathBuf::from("/audio/a.wav"), PathBuf::from("/audio/b.wav")];

        // Named like a subtitle file, whether or not it exists yet.
        assert_eq!(
            resolve_outputs(&inputs, Some(Path::new("/out.srt")), ExportFormat::Srt)
                .expect_err("a file is not a directory")
                .code,
            2
        );
        // And stdout is not a directory either.
        assert_eq!(
            resolve_outputs(&inputs, Some(Path::new("-")), ExportFormat::Srt)
                .expect_err("stdout is not a directory")
                .code,
            2
        );
    }
}
