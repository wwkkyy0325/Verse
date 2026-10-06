//! Verse command line.
//!
//! Phase 1 entry point: fetch models, transcribe files, offline.
//!
//! Argument parsing is hand-rolled. The surface is small and stable, and the
//! project's whole point is a small footprint — pulling in an argument parser
//! for a handful of flags would be out of proportion.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use verse_asr::register_builtin_engines;
use verse_audio::{FfmpegDecoder, SileroVad};
use verse_core::{
    AsrEngine, AudioChunk, AudioFormat, AudioSource, CancelToken, EngineConfig, ExportFormat,
    HardwareProfile, Registry, Segment, SegmentId, Segmenter, Transcript,
};
use verse_model::{Catalog, DownloadState, Downloader};

const DEFAULT_ENGINE: &str = "sensevoice";
const DEFAULT_MODELS_DIR: &str = "models";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let result = match args.first().map(String::as_str) {
        Some("transcribe") => TranscribeOptions::parse(&args[1..])
            .and_then(|options| transcribe(&options).map_err(|e| e.to_string())),
        Some("model") => model_command(&args[1..]),
        Some("--version") | Some("-V") => {
            println!("verse {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help") | Some("-h") | None => {
            print_usage();
            Ok(())
        }
        Some(other) => Err(format!("unknown command '{other}'\n\nrun 'verse --help'")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
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
    input: PathBuf,
    output: Option<PathBuf>,
    format: Option<ExportFormat>,
    engine: String,
    models_dir: PathBuf,
    vad: Option<PathBuf>,
}

impl TranscribeOptions {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self {
            input: PathBuf::new(),
            output: None,
            format: None,
            engine: DEFAULT_ENGINE.to_string(),
            models_dir: PathBuf::from(DEFAULT_MODELS_DIR),
            vad: None,
        };
        let mut input: Option<PathBuf> = None;

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
                            .ok_or_else(|| format!("unknown format '{raw}'"))?,
                    );
                }
                "--engine" => options.engine = take_value(args, &mut i, "--engine")?,
                "--models" => {
                    options.models_dir = PathBuf::from(take_value(args, &mut i, "--models")?)
                }
                "--vad" => options.vad = Some(PathBuf::from(take_value(args, &mut i, "--vad")?)),
                other if other.starts_with('-') => {
                    return Err(format!(
                        "unknown option '{other}'\n\n{}",
                        transcribe_usage()
                    ));
                }
                other => {
                    if input.is_some() {
                        return Err(format!("unexpected extra argument '{other}'"));
                    }
                    input = Some(PathBuf::from(other));
                }
            }
            i += 1;
        }

        options.input =
            input.ok_or_else(|| format!("no input file given\n\n{}", transcribe_usage()))?;
        Ok(options)
    }

    /// Where this run's output goes, and in what format.
    fn resolve_output(&self) -> (PathBuf, ExportFormat) {
        let format = self.format.or_else(|| {
            self.output
                .as_ref()
                .and_then(|p| p.extension())
                .and_then(|e| e.to_str())
                .and_then(ExportFormat::from_extension)
        });

        match (&self.output, format) {
            (Some(path), Some(format)) => (path.clone(), format),
            (Some(path), None) => (path.clone(), ExportFormat::Srt),
            (None, format) => {
                let format = format.unwrap_or(ExportFormat::Srt);
                (self.input.with_extension(format.extension()), format)
            }
        }
    }
}

fn transcribe_usage() -> String {
    let mut s = String::from("usage: verse transcribe <file> [options]\n\noptions:\n");
    s.push_str("  -o, --output <file>     Output file (default: input name with .srt)\n");
    s.push_str("      --format <srt|txt>  Output format (default: from the file extension)\n");
    s.push_str(&format!(
        "      --engine <id>       Engine (default: {DEFAULT_ENGINE})\n"
    ));
    s.push_str(&format!(
        "      --models <dir>      Model root (default: {DEFAULT_MODELS_DIR})\n"
    ));
    s.push_str("      --punctuation       Add punctuation (needed by engines without it)\n");
    s.push_str(
        "      --vad <file>        VAD model (default: <models>/silero-vad/silero_vad.onnx)",
    );
    s
}

fn transcribe(options: &TranscribeOptions) -> Result<(), Box<dyn std::error::Error>> {
    // Reported once, and only when something is being worked around.
    let hardware = HardwareProfile::probe();
    if let Some(notice) = hardware.tier().notice() {
        eprintln!("{notice}");
    }

    let model_dir = options.models_dir.join(&options.engine);

    let mut registry = Registry::new();
    register_builtin_engines(&mut registry);
    let mut engine = registry.create_engine(
        &options.engine,
        &EngineConfig {
            model_dir,
            threads: hardware.engine_threads(),
            inverse_text_normalization: true,
        },
    )?;

    let vad_model = options.vad.clone().unwrap_or_else(|| {
        options
            .models_dir
            .join("silero-vad")
            .join("silero_vad.onnx")
    });
    let mut vad = SileroVad::load(&vad_model, AudioFormat::TARGET)?;

    let mut source = FfmpegDecoder::open(&options.input, AudioFormat::TARGET)?;
    let mut transcript = Transcript::default();
    let mut spans = 0usize;

    while let Some(chunk) = source.next_chunk()? {
        vad.accept(&chunk)?;
        spans += recognize(vad.take(), engine.as_mut(), &mut transcript)?;
    }
    spans += recognize(vad.finish(), engine.as_mut(), &mut transcript)?;

    let (path, format) = options.resolve_output();
    std::fs::write(&path, format.render(&transcript))?;

    eprintln!(
        "{spans} spans -> {} segments -> {}",
        transcript.segments.len(),
        path.display()
    );
    Ok(())
}

/// Reuse one engine across spans, one span at a time.
///
/// This is what bounds memory: each span is recognized and dropped before the
/// next is buffered, so a long recording never exists in memory all at once.
fn recognize(
    spans: Vec<AudioChunk>,
    engine: &mut dyn AsrEngine,
    out: &mut Transcript,
) -> Result<usize, Box<dyn std::error::Error>> {
    let count = spans.len();

    for span in spans {
        engine.reset();
        engine.accept(&span)?;
        let piece = engine.finalize()?;

        // Each span's engine reports segment id 0; renumber so ids stay unique
        // across the whole transcript.
        for segment in piece.segments {
            let id = SegmentId(out.segments.len() as u64);
            out.segments.push(Segment { id, ..segment });
        }
    }

    Ok(count)
}

// --------------------------------------------------------------------- model

fn model_command(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("list") => model_list(&models_dir_from(args)),
        Some("fetch") => {
            let id = args
                .get(1)
                .filter(|a| !a.starts_with('-'))
                .ok_or("usage: verse model fetch <id> [--models <dir>]")?;
            model_fetch(id, &models_dir_from(args))
        }
        _ => Err("usage: verse model list | verse model fetch <id>".to_string()),
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

fn load_catalog(models_dir: &Path) -> Result<Catalog, String> {
    Catalog::load_or_embedded(&catalog_path(models_dir)).map_err(|e| e.message().to_string())
}

fn model_list(models_dir: &Path) -> Result<(), String> {
    let catalog = load_catalog(models_dir)?;

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

fn model_fetch(id: &str, models_dir: &Path) -> Result<(), String> {
    let catalog = load_catalog(models_dir)?;

    let spec = catalog
        .find(id)
        .ok_or_else(|| format!("unknown model '{id}'; run 'verse model list'"))?;

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
        .map_err(|e| e.message().to_string())?;

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
        DownloadState::Failed { reason } => Err(reason),
        other => Err(format!("unexpected end state: {other:?}")),
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
fn take_value(args: &[String], i: &mut usize, name: &str) -> Result<String, String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| format!("{name} needs a value"))
}
