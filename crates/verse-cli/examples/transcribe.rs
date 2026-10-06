//! Manual smoke check for one engine in isolation: file -> ffmpeg decode ->
//! engine -> text.
//!
//! Deliberately goes through the registry and the traits rather than reaching
//! into sherpa-onnx, so it exercises the same path the pipeline will. It also
//! deliberately *skips* the voice detector, feeding the whole file to the
//! engine at once — the only way to tell a recogniser that failed from a
//! recogniser that was never given the audio.
//!
//! Run:
//!   cargo run -p verse-cli --example transcribe -- \
//!       <engine-id> <model-dir> <audio-file> 
//!
//! `engine-id` is `sensevoice` or `qwen3-asr`.

use std::path::{Path, PathBuf};

use verse_asr::register_builtin_engines;
use verse_audio::FfmpegDecoder;
use verse_core::{AudioFormat, AudioSource, EngineConfig, Registry};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("usage: transcribe <engine-id> <model-dir> <audio-file> ");
        eprintln!("       engine-id: sensevoice | qwen3-asr");
        std::process::exit(2);
    }

    let engine_id = &args[0];
    let model_dir = PathBuf::from(&args[1]);
    let audio = &args[2];

    // Assemble the available engines. Nothing below this line knows which
    // engine it got.
    let mut registry = Registry::new();
    register_builtin_engines(&mut registry);

    let mut engine = registry.create_engine(
        engine_id,
        &EngineConfig {
            model_dir,
            threads: 4,
            inverse_text_normalization: true,
            max_output_tokens: None,
            hotwords: None,
        },
    )?;

    let mut source = FfmpegDecoder::open(Path::new(audio), AudioFormat::TARGET)?;

    let mut chunks = 0usize;
    while let Some(chunk) = source.next_chunk()? {
        engine.accept(&chunk)?;
        chunks += 1;
    }

    let transcript = engine.finalize()?;

    eprintln!(
        "decoded {chunks} chunks, {} segment(s)",
        transcript.segments.len()
    );

    println!("{}", transcript.to_text());
    Ok(())
}
