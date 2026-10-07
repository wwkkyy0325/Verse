//! Verse command line.
//!
//! Phase 1 entry point: fetch models, transcribe files, offline.
//!
//! Argument parsing is hand-rolled. The surface is small and stable, and the
//! project's whole point is a small footprint — pulling in an argument parser
//! for a handful of flags would be out of proportion.

mod report;
mod serve;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use verse_core::{CancelToken, ErrorKind, EventBus, ExportFormat, HardwareProfile, JobId};
use verse_model::{Catalog, DownloadState, Downloader};
use verse_pipeline::{GuardSettings, Request, Transcriber};
use verse_store::{FileId, Ownership};

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
        Some("cache") => cache_command(&args[1..]),
        Some("serve") => serve_command(&args[1..]),
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
    println!("  model list            Show known models, with their sizes");
    println!("  model fetch <id>      Download a model");
    println!("  model remove <id>     Delete an installed model");
    println!("  model clean           Delete half-finished downloads");
    println!("  serve                 Listen on 127.0.0.1 so other programs can drive it");
    println!("  cache size            Show what the transcription cache holds");
    println!("  cache clean           Empty the transcription cache");
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
    /// Skip the result cache entirely.
    ///
    /// For timing a real run, and for the case where a cached transcript is
    /// being distrusted. `VERSE_NO_CACHE` does the same thing for a caller who
    /// cannot change the command.
    no_cache: bool,
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
            no_cache: false,
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
                "--no-cache" => options.no_cache = true,
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

/// Whether this run keeps and consults a transcript cache, and where.
///
/// The flag wins over the environment. A caller who typed `--no-cache` meant
/// it, while `VERSE_NO_CACHE` left set in a shell profile is exactly the kind
/// of thing one forgets having done.
fn cache_policy(no_cache: bool) -> verse_pipeline::CachePolicy {
    if no_cache || verse_store::caching_refused() {
        return verse_pipeline::CachePolicy::Disabled;
    }
    verse_pipeline::CachePolicy::under(&verse_store::data_dir(&verse_store::Roots::from_env()))
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
    default_dir: &Path,
    ownership: &mut Ownership,
) -> Result<Vec<PathBuf>, Failure> {
    let stdout = Path::new("-");

    // One input and an explicit path: that path, verbatim. Nothing here has an
    // opinion about a name the caller chose.
    if let ([_only], Some(path)) = (inputs, output) {
        return Ok(vec![path.to_path_buf()]);
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

    let dir = output.unwrap_or(default_dir);

    let mut planned = Vec::with_capacity(inputs.len());
    for input in inputs {
        let stem = input.file_stem().unwrap_or_default();

        // Named through the ownership record when the input can be identified,
        // so that a re-run replaces its own transcript and a *different*
        // recording with the same name gets a numbered one instead of
        // destroying somebody else's work. That matters far more here than it
        // did when output sat beside its input, because one output directory
        // holds every recording the user has.
        match FileId::of(input) {
            Ok(source) => {
                let choice =
                    verse_store::destination(dir, stem, format.extension(), &source, ownership);
                ownership.record(&source, &choice.path);

                if let Some(n) = choice.serial {
                    // Said out loud. A filename that changed without anybody
                    // being told is how a script silently starts reading the
                    // wrong file.
                    eprintln!(
                        "{} already has a transcript here; writing {} ({n})",
                        input.display(),
                        choice.path.display()
                    );
                }

                planned.push(choice.path);
            }
            // Cannot identify it, so cannot claim a name for it. A plain name
            // is the safe answer: it is what the caller would have got before
            // any of this existed.
            Err(_) => planned.push(
                dir.join(stem)
                    .with_extension(format.extension()),
            ),
        }
    }

    Ok(planned)
}

fn transcribe_usage() -> String {
    let mut s = String::from("usage: verse transcribe <file|dir>... [options]\n\noptions:\n");
    s.push_str("  -o, --output <path>    Output file, or a directory for several inputs\n");
    s.push_str(&format!(
        "                         (default: {})\n",
        verse_store::output_dir(&verse_store::Roots::from_env()).display()
    ));
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
    s.push_str("      --no-cache         Recognise even if the result is already cached\n");
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

    let roots = verse_store::Roots::from_env();
    let data_dir = verse_store::data_dir(&roots);
    let default_dir = verse_store::output_dir(&roots);

    // Said once, and only about a location the user did not choose. OneDrive
    // redirection is followed — that is where their Documents are — but a
    // transcript landing in a folder a background service uploads is a fact
    // about their machine they are entitled to hear rather than deduce.
    if options.output.is_none() {
        for notice in verse_store::notices(&roots) {
            eprintln!("note: {notice}");
        }
    }

    let record_path = data_dir.join("outputs.json");
    // Forgotten files are dropped on the way in, never on the way out. Doing it
    // before a save would take every claim made moments earlier with it, since
    // none of those transcripts has been written yet — which is precisely what
    // happened the first time this was tried, and why the output directory
    // filled with `zh.srt`, `zh (2).srt`, `zh (3).srt`.
    let mut ownership = Ownership::load(&record_path);
    ownership.prune();

    let outputs =
        resolve_outputs(&inputs, options.output.as_deref(), format, &default_dir, &mut ownership)?;

    // Written before the transcript is, so the claim is durable even if the run
    // is interrupted.
    if let Err(e) = ownership.save(&record_path) {
        // Not fatal — the transcripts still get written — but it is the
        // difference between replacing a file and accumulating numbered
        // copies, so it is said rather than swallowed.
        eprintln!(
            "warning: could not record where transcripts go ({}): {e}",
            record_path.display()
        );
    }

    // Each output's own directory, so the default directory is made on first
    // use and an explicit one is made when several inputs need it. `-o -` has
    // no directory and is not a file.
    for path in &outputs {
        if path == Path::new("-") {
            continue;
        }
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| Failure {
                    code: exit_code(ErrorKind::Sink),
                    message: format!("could not create {}: {e}", parent.display()),
                })?;
            }
        }
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
        cache: cache_policy(options.no_cache),
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

    // The same rule the service's pool uses, so the two cannot disagree about
    // what a worker asks the machine for.
    let per_worker_threads = verse_core::per_worker_threads(hardware.engine_threads(), workers);

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
    const USAGE: &str =
        "usage: verse model list | fetch <id> | remove <id> | clean [--models <dir>] [--json]";

    match args.first().map(String::as_str) {
        Some("list") => model_list(&models_dir_from(args), args.iter().any(|a| a == "--json")),
        Some("fetch") => {
            let id = args
                .get(1)
                .filter(|a| !a.starts_with('-'))
                .ok_or_else(|| Failure::usage("usage: verse model fetch <id> [--models <dir>]"))?;
            model_fetch(id, &models_dir_from(args))
        }
        Some("remove") => {
            let id = args
                .get(1)
                .filter(|a| !a.starts_with('-'))
                .ok_or_else(|| Failure::usage("usage: verse model remove <id> [--models <dir>]"))?;
            model_remove(id, &models_dir_from(args))
        }
        Some("clean") => model_clean(&models_dir_from(args), args.iter().any(|a| a == "--json")),
        _ => Err(Failure::usage(USAGE)),
    }
}

/// Delete an installed model.
///
/// Reports what it freed, because "removed" on its own does not tell anyone
/// whether it was worth doing.
fn model_remove(id: &str, models_dir: &Path) -> Result<(), Failure> {
    let freed = verse_model::cleanup::remove_model(models_dir, id).map_err(|e| Failure {
        code: if e.kind() == std::io::ErrorKind::NotFound {
            exit_code(ErrorKind::Model)
        } else {
            exit_code(ErrorKind::Io)
        },
        message: format!("could not remove {id}: {e}"),
    })?;

    println!("removed {id} ({})", human_bytes(freed));
    Ok(())
}

/// Delete half-finished downloads.
///
/// The downloader keeps them deliberately, so that an interrupted transfer can
/// be resumed. They become litter only once nobody is going to resume them,
/// which is not a thing this can determine — so it is a command rather than
/// something done at startup.
fn model_clean(models_dir: &Path, json: bool) -> Result<(), Failure> {
    let found = verse_model::cleanup::partials(models_dir);
    let removed = verse_model::cleanup::remove_partials(models_dir).map_err(|e| Failure {
        code: exit_code(ErrorKind::Io),
        message: format!("could not clean {}: {e}", models_dir.display()),
    })?;

    if json {
        report::print_json(&report::Cleanup {
            version: report::VERSION,
            removed: found.iter().map(|p| p.path.display().to_string()).collect(),
            bytes: removed.bytes,
        });
        return Ok(());
    }

    if found.is_empty() {
        println!("nothing to clean in {}", models_dir.display());
        return Ok(());
    }

    for partial in &found {
        println!("  removed {}", partial.path.display());
    }
    println!("{} files, {}", removed.files, human_bytes(removed.bytes));
    Ok(())
}

/// Report and clear the transcription cache.
fn cache_command(args: &[String]) -> Result<(), Failure> {
    const USAGE: &str = "usage: verse cache size | clean [--json]";

    let cache = verse_store::Cache::under(&verse_store::data_dir(&verse_store::Roots::from_env()));
    let json = args.iter().any(|a| a == "--json");

    match args.first().map(String::as_str) {
        Some("size") => {
            let usage = cache.usage();
            if json {
                report::print_json(&report::CacheUsage {
                    version: report::VERSION,
                    directory: cache.root().display().to_string(),
                    entries: usage.entries,
                    bytes: usage.bytes,
                    temporary_bytes: usage.temporary_bytes,
                });
            } else {
                println!("cache: {}", cache.root().display());
                println!(
                    "  {} transcriptions, {}",
                    usage.entries,
                    human_bytes(usage.bytes)
                );
                if usage.temporaries > 0 {
                    println!(
                        "  {} half-written files, {}",
                        usage.temporaries,
                        human_bytes(usage.temporary_bytes)
                    );
                }
            }
            Ok(())
        }
        Some("clean") => {
            // Swept first: a half-written file from a process that died is
            // removed whether or not it is still recent enough for the sweep
            // to have taken it on its own.
            let swept = cache.sweep_temporaries();
            let removed = cache.clean().map_err(|e| Failure {
                code: exit_code(ErrorKind::Io),
                message: format!("could not clean the cache: {e}"),
            })?;

            if json {
                report::print_json(&report::Cleanup {
                    version: report::VERSION,
                    removed: (0..removed.entries)
                        .map(|n| format!("entry {n}"))
                        .collect(),
                    bytes: removed.total_bytes(),
                });
            } else {
                println!(
                    "cleared {} transcriptions, {}{}",
                    removed.entries,
                    human_bytes(removed.total_bytes()),
                    if swept > 0 {
                        format!(" ({swept} half-written files)")
                    } else {
                        String::new()
                    }
                );
            }
            Ok(())
        }
        _ => Err(Failure::usage(USAGE)),
    }
}

/// Run the loopback service until killed.
///
/// The listener binds `127.0.0.1` and only that, and it initiates no
/// connection: `design.md` §4.5 carries the argument for why that does not
/// weaken the offline guarantee.
fn serve_command(args: &[String]) -> Result<(), Failure> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{}", serve_usage());
        return Ok(());
    }

    const USAGE: &str = "usage: verse serve [--port <n>] [--models <dir>] [--engine <id>]";

    let mut config = serve::Config {
        port: serve::DEFAULT_PORT,
        models_dir: PathBuf::from(DEFAULT_MODELS_DIR),
        engine: DEFAULT_ENGINE.to_string(),
        idle: serve::Config::idle_from_env(),
        workers: None,
    };

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                let raw = take_value(args, &mut i, "--port")?;
                config.port = raw.parse().map_err(|_| {
                    Failure::usage(format!("--port wants a number, not {raw:?}"))
                })?;
            }
            "--models" => {
                config.models_dir = PathBuf::from(take_value(args, &mut i, "--models")?);
            }
            "--engine" => config.engine = take_value(args, &mut i, "--engine")?,
            "--workers" => {
                let raw = take_value(args, &mut i, "--workers")?;
                let count: usize = raw.parse().map_err(|_| {
                    Failure::usage(format!("--workers wants a number, not {raw:?}"))
                })?;
                // Refused rather than silently corrected, the way `-j 0` is: a
                // service that cannot run any job is not what was asked for.
                if count == 0 {
                    return Err(Failure::usage("--workers must be at least 1"));
                }
                config.workers = Some(count);
            }
            "--no-idle" => config.idle = None,
            other => return Err(Failure::usage(format!("unknown option {other:?}\n\n{USAGE}"))),
        }
        // `take_value` stops *on* the value, so the step past it is here — the
        // same shape every other parser in this file uses.
        i += 1;
    }

    // The environment overrides the default, and the flag overrides that, which
    // is the order every other setting in this program uses.
    if config.port == serve::DEFAULT_PORT {
        if let Some(raw) = std::env::var_os("VERSE_SERVE_PORT") {
            let text = raw.to_string_lossy().into_owned();
            if !text.is_empty() {
                config.port = text.trim().parse().map_err(|_| {
                    Failure::usage(format!("VERSE_SERVE_PORT={text:?} is not a port number"))
                })?;
            }
        }
    }
    if let Ok(raw) = std::env::var("VERSE_MODELS") {
        if !raw.is_empty() && config.models_dir == Path::new(DEFAULT_MODELS_DIR) {
            config.models_dir = PathBuf::from(raw);
        }
    }

    serve::run(config).map_err(|message| Failure {
        code: exit_code(ErrorKind::Io),
        message,
    })
}

fn serve_usage() -> String {
    let mut s = String::from("usage: verse serve [options]\n\n");
    s.push_str("Runs until killed, listening on 127.0.0.1 only. Other programs on this\n");
    s.push_str("machine can submit transcription jobs and poll for the result.\n\n");
    s.push_str("options:\n");
    s.push_str(&format!(
        "      --port <n>         Port (default {}). 0 asks the system for a free one.\n",
        serve::DEFAULT_PORT
    ));
    s.push_str(&format!(
        "      --models <dir>     Model root (default {DEFAULT_MODELS_DIR})\n"
    ));
    s.push_str(&format!(
        "      --engine <id>      Engine (default {DEFAULT_ENGINE})\n"
    ));
    s.push_str("      --workers <n>      Jobs to recognise at once (default 1 for now).\n");
    s.push_str("                         Each worker holds its own model: about 250 MB for\n");
    s.push_str("                         SenseVoice, 1 GB for Qwen3.\n");
    s.push_str("      --no-idle          Keep the model loaded until killed\n");
    s.push_str("\nenvironment:\n");
    s.push_str("      VERSE_SERVE_PORT   The same as --port\n");
    s.push_str("      VERSE_MODELS       The same as --models\n");
    s.push_str("      VERSE_MODEL_IDLE_SECS\n");
    s.push_str("                         Seconds to keep the model after the last job\n");
    s.push_str("                         (default 300; 0 keeps it for the process's life)\n");
    s.push_str("\nThe port, the token and the pid are written to serve.json in the per-user\n");
    s.push_str("data directory, which is where a client should look for them.\n");
    s
}

/// Bytes as something a person reads.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
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
                bytes: verse_model::cleanup::usage(models_dir, &spec.id).bytes,
                partial_bytes: verse_model::cleanup::partials(
                    &Downloader::directory_for(spec, models_dir),
                )
                .iter()
                .map(|partial| partial.bytes)
                .sum(),
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
        let installed = Downloader::is_present(spec, models_dir);
        let mark = if installed { "present" } else { "missing" };

        let on_disk = verse_model::cleanup::usage(models_dir, &spec.id).bytes;
        let partial: u64 =
            verse_model::cleanup::partials(&Downloader::directory_for(spec, models_dir))
                .iter()
                .map(|p| p.bytes)
                .sum();

        println!(
            "  {:<12} {:<8} {:>10} {}",
            spec.id,
            mark,
            if on_disk > 0 {
                human_bytes(on_disk)
            } else {
                "-".to_string()
            },
            spec.display_name
        );

        // Said separately, because a directory holding 500 MB of abandoned
        // transfer is not a model that is 500 MB on its way to being installed.
        if partial > 0 {
            println!("      {} of it is an unfinished download", human_bytes(partial));
        }

        if !installed {
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
    fn the_usage_text_names_the_default_output_directory() {
        // The help is computed from the same function the resolution uses, and
        // this pins that. A default nobody can find without reading the source
        // is a default that surprises people.
        let usage = transcribe_usage();
        let expected = verse_store::output_dir(&verse_store::Roots::from_env());

        assert!(
            usage.contains(&expected.display().to_string()),
            "the help must say where output goes; got:\n{usage}"
        );
    }

    /// A real file, because the naming rules key on what is actually on disk.
    fn audio(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&path, b"pretend audio").expect("write");
        path
    }

    #[test]
    fn one_input_with_an_explicit_path_uses_it_verbatim() {
        let dir = scratch("resolve-explicit");
        let inputs = vec![audio(&dir, "a.m4a")];
        let mut record = Ownership::default();

        let outputs = resolve_outputs(
            &inputs,
            Some(Path::new("/tmp/out.txt")),
            ExportFormat::Srt,
            &dir.join("default"),
            &mut record,
        )
        .expect("resolve");

        assert_eq!(outputs, vec![PathBuf::from("/tmp/out.txt")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_input_with_no_output_goes_to_the_default_directory() {
        // The change: output no longer lands beside the recording.
        let dir = scratch("resolve-default");
        let inputs = vec![audio(&dir, "meeting.m4a")];
        let mut record = Ownership::default();
        let default = dir.join("Documents/Verse");

        let outputs = resolve_outputs(&inputs, None, ExportFormat::Srt, &default, &mut record)
            .expect("resolve");

        assert_eq!(outputs, vec![default.join("meeting.srt")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn several_inputs_write_into_the_directory_named() {
        let dir = scratch("resolve-dir");
        let inputs = vec![audio(&dir, "one/a.wav"), audio(&dir, "two/b.wav")];
        let mut record = Ownership::default();
        let out = dir.join("out");

        let outputs = resolve_outputs(&inputs, Some(&out), ExportFormat::Srt, &out, &mut record)
            .expect("resolve");

        assert_eq!(outputs, vec![out.join("a.srt"), out.join("b.srt")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_inputs_with_one_name_get_numbers_rather_than_a_clash() {
        // `a.wav` and `a.mp3` both want `a.srt`. Numbered rather than refused,
        // because with one output directory this is the ordinary case — two
        // recordings really can be called 会议.m4a — and refusing would make
        // `verse transcribe recordings/` unusable.
        let dir = scratch("resolve-numbered");
        let inputs = vec![audio(&dir, "one/a.wav"), audio(&dir, "two/a.mp3")];
        let mut record = Ownership::default();
        let out = dir.join("out");

        let outputs = resolve_outputs(&inputs, Some(&out), ExportFormat::Srt, &out, &mut record)
            .expect("resolve");

        assert_eq!(outputs, vec![out.join("a.srt"), out.join("a (2).srt")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn running_the_same_batch_twice_names_the_same_files() {
        // Otherwise a second run fills the folder with copies, which is the
        // failure this is most likely to have.
        let dir = scratch("resolve-idempotent");
        let inputs = vec![audio(&dir, "one/a.wav"), audio(&dir, "two/a.mp3")];
        let out = dir.join("out");
        std::fs::create_dir_all(&out).expect("out dir");
        let mut record = Ownership::default();

        let first = resolve_outputs(&inputs, Some(&out), ExportFormat::Srt, &out, &mut record)
            .expect("resolve");
        for path in &first {
            std::fs::write(path, "text").expect("write");
        }

        let second = resolve_outputs(&inputs, Some(&out), ExportFormat::Srt, &out, &mut record)
            .expect("resolve");

        assert_eq!(first, second);
        assert_eq!(std::fs::read_dir(&out).expect("list").count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn somebody_elses_transcript_is_never_replaced() {
        // Through the default directory rather than `-o`: with a single input,
        // `-o <path>` names a file, which is the rule this command has always
        // had and which `one_input_with_an_explicit_path_uses_it_verbatim`
        // pins.
        let dir = scratch("resolve-foreign");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).expect("out dir");
        std::fs::write(out.join("a.srt"), "typed by hand").expect("plant");

        let inputs = vec![audio(&dir, "a.wav")];
        let mut record = Ownership::default();

        let outputs = resolve_outputs(&inputs, None, ExportFormat::Srt, &out, &mut record)
            .expect("resolve");

        assert_eq!(outputs, vec![out.join("a (2).srt")]);
        assert_eq!(
            std::fs::read_to_string(out.join("a.srt")).expect("read"),
            "typed by hand"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_output_file_cannot_hold_several_transcripts() {
        let dir = scratch("resolve-file-for-many");
        let inputs = vec![audio(&dir, "a.wav"), audio(&dir, "b.wav")];
        let mut record = Ownership::default();

        // Named like a subtitle file, whether or not it exists yet.
        assert_eq!(
            resolve_outputs(
                &inputs,
                Some(Path::new("/out.srt")),
                ExportFormat::Srt,
                &dir,
                &mut record
            )
            .expect_err("a file is not a directory")
            .code,
            2
        );
        // And stdout is not a directory either.
        assert_eq!(
            resolve_outputs(
                &inputs,
                Some(Path::new("-")),
                ExportFormat::Srt,
                &dir,
                &mut record
            )
            .expect_err("stdout is not a directory")
            .code,
            2
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
