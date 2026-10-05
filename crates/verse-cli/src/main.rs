//! Verse command line.
//!
//! Phase 1 entry point: transcribe a file, offline.
//!
//! Argument parsing is hand-rolled. The surface is small and stable, and the
//! project's whole point is a small footprint — pulling in an argument parser
//! for four flags would be out of proportion.

use std::path::PathBuf;
use std::process::ExitCode;

use verse_asr::{register_builtin_engines, Punctuator};
use verse_audio::{FfmpegDecoder, SileroVad};
use verse_core::{
    AsrEngine, AudioChunk, AudioFormat, AudioSource, EngineConfig, ExportFormat, Registry, Segment,
    SegmentId, Segmenter, TextChain, Transcript,
};

const DEFAULT_ENGINE: &str = "sensevoice";
const DEFAULT_VAD: &str = "models/vad/silero_vad.onnx";
const DEFAULT_MODEL_ROOT: &str = "models";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("transcribe") => match Options::parse(&args[1..]) {
            Ok(options) => match run(&options) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::FAILURE
                }
            },
            Err(message) => {
                eprintln!("error: {message}");
                eprintln!();
                print_transcribe_usage();
                ExitCode::from(2)
            }
        },
        Some("--version") | Some("-V") => {
            println!("verse {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help") | Some("-h") | None => {
            print_usage();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("error: unknown command '{other}'");
            eprintln!();
            print_usage();
            ExitCode::from(2)
        }
    }
}

fn print_usage() {
    println!("verse {}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("usage: verse <command> [options]");
    println!();
    println!("commands:");
    println!("  transcribe <file>   Transcribe an audio or video file");
    println!();
    println!("  --help, -h          Show this message");
    println!("  --version, -V       Show the version");
}

fn print_transcribe_usage() {
    println!("usage: verse transcribe <file> [options]");
    println!();
    println!("options:");
    println!("  -o, --output <file>     Output file (default: input name with .srt)");
    println!("      --format <srt|txt>   Output format (default: from the file extension)");
    println!("      --engine <id>        Recognition engine (default: {DEFAULT_ENGINE})");
    println!("      --model-dir <dir>    Model directory (default: {DEFAULT_MODEL_ROOT}/<engine>)");
    println!("      --punct <file>       Punctuation model, if the engine needs one");
    println!("      --vad <file>         VAD model (default: {DEFAULT_VAD})");
}

struct Options {
    input: PathBuf,
    output: Option<PathBuf>,
    format: Option<ExportFormat>,
    engine: String,
    model_dir: Option<PathBuf>,
    punct: Option<PathBuf>,
    vad: PathBuf,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut input: Option<PathBuf> = None;
        let mut output = None;
        let mut format = None;
        let mut engine = DEFAULT_ENGINE.to_string();
        let mut model_dir = None;
        let mut punct = None;
        let mut vad = PathBuf::from(DEFAULT_VAD);

        let mut i = 0;
        while i < args.len() {
            let arg = args[i].clone();

            match arg.as_str() {
                "-o" | "--output" => {
                    output = Some(PathBuf::from(take_value(args, &mut i, "--output")?))
                }
                "--format" => {
                    let raw = take_value(args, &mut i, "--format")?;
                    format = Some(
                        ExportFormat::from_extension(&raw)
                            .ok_or_else(|| format!("unknown format '{raw}'"))?,
                    );
                }
                "--engine" => engine = take_value(args, &mut i, "--engine")?,
                "--model-dir" => {
                    model_dir = Some(PathBuf::from(take_value(args, &mut i, "--model-dir")?))
                }
                "--punct" => punct = Some(PathBuf::from(take_value(args, &mut i, "--punct")?)),
                "--vad" => vad = PathBuf::from(take_value(args, &mut i, "--vad")?),
                other if other.starts_with('-') => {
                    return Err(format!("unknown option '{other}'"));
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

        Ok(Self {
            input: input.ok_or("no input file given")?,
            output,
            format,
            engine,
            model_dir,
            punct,
            vad,
        })
    }

    /// Where this run's output goes.
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

/// Consume the value that follows a flag.
fn take_value(args: &[String], i: &mut usize, name: &str) -> Result<String, String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| format!("{name} needs a value"))
}

fn run(options: &Options) -> Result<(), Box<dyn std::error::Error>> {
    let mut registry = Registry::new();
    register_builtin_engines(&mut registry);

    let mut engine = registry.create_engine(
        &options.engine,
        &EngineConfig {
            model_dir: options
                .model_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from(DEFAULT_MODEL_ROOT).join(&options.engine)),
            threads: engine_threads(),
        },
    )?;

    let mut vad = SileroVad::load(&options.vad, AudioFormat::TARGET)?;
    let mut source = FfmpegDecoder::open(&options.input, AudioFormat::TARGET)?;

    let mut transcript = Transcript::default();
    let mut spans = 0usize;

    while let Some(chunk) = source.next_chunk()? {
        vad.accept(&chunk)?;
        spans += recognize(vad.take(), engine.as_mut(), &mut transcript)?;
    }
    spans += recognize(vad.finish(), engine.as_mut(), &mut transcript)?;

    if let Some(model) = &options.punct {
        let mut chain = TextChain::new();
        chain.push(Box::new(Punctuator::load(model)?));
        chain.run_transcript(&mut transcript);
    }

    let (path, format) = options.resolve_output();
    std::fs::write(&path, format.render(&transcript))?;

    eprintln!(
        "{} spans -> {} segments -> {}",
        spans,
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

/// Leave one core for the UI and whatever else the machine is doing.
fn engine_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).max(1))
        .unwrap_or(1)
}
