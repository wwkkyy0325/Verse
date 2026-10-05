//! Manual smoke check for the full chain:
//! file -> ffmpeg decode -> engine -> optional punctuation -> text.
//!
//! Deliberately goes through the registry and the traits rather than reaching
//! into sherpa-onnx, so it exercises the same path a pipeline will.
//!
//! Run:
//!   cargo run -p verse-cli --example transcribe -- \
//!       <engine-id> <model-dir> <audio-file> [punct-model]
//!
//! `engine-id` is `paraformer` or `sensevoice`.

use std::path::{Path, PathBuf};

use verse_asr::{register_builtin_engines, Punctuator};
use verse_audio::FfmpegDecoder;
use verse_core::{AudioFormat, AudioSource, EngineConfig, Registry, TextChain};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("usage: transcribe <engine-id> <model-dir> <audio-file> [punct-model]");
        eprintln!("       engine-id: paraformer | sensevoice");
        std::process::exit(2);
    }

    let engine_id = &args[0];
    let model_dir = PathBuf::from(&args[1]);
    let audio = &args[2];
    let punct_model = args.get(3);

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
        },
    )?;

    let mut source = FfmpegDecoder::open(Path::new(audio), AudioFormat::TARGET)?;

    let mut chunks = 0usize;
    while let Some(chunk) = source.next_chunk()? {
        engine.accept(&chunk)?;
        chunks += 1;
    }

    let mut transcript = engine.finalize()?;

    eprintln!(
        "decoded {chunks} chunks, {} segment(s)",
        transcript.segments.len()
    );

    // Punctuation is a chain stage, not a hard-coded step. SenseVoice needs no
    // stage here; Paraformer does.
    if let Some(model) = punct_model {
        let mut chain = TextChain::new();
        chain.push(Box::new(Punctuator::load(Path::new(model))?));
        eprintln!("text stages: {:?}", chain.names());
        chain.run_transcript(&mut transcript);
    }

    println!("{}", transcript.to_text());
    Ok(())
}
