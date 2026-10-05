//! Manual smoke check: transcribe a WAV file with a Paraformer model.
//!
//! Deliberately not part of the test suite — it needs model files that are
//! downloaded separately and never committed (see `design.md` §6).
//!
//! Run:
//!   cargo run -p verse-asr --example transcribe_paraformer -- \
//!       <model.onnx> <tokens.txt> <audio.wav> [punct.onnx]
//!
//! The optional fourth argument is a punctuation model. Without it the output
//! is bare text, which is what the recognizer alone produces.

use std::path::Path;

use sherpa_onnx::{
    OfflineModelConfig, OfflineParaformerModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    Wave,
};
use verse_asr::Punctuator;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 3 && args.len() != 4 {
        eprintln!(
            "usage: transcribe_paraformer <model.onnx> <tokens.txt> <audio.wav> [punct.onnx]"
        );
        std::process::exit(2);
    }
    let (model, tokens, wav_path) = (&args[0], &args[1], &args[2]);
    let punct_model = args.get(3);

    let config = OfflineRecognizerConfig {
        model_config: OfflineModelConfig {
            paraformer: OfflineParaformerModelConfig {
                model: Some(model.clone()),
            },
            tokens: Some(tokens.clone()),
            num_threads: 2,
            ..Default::default()
        },
        ..Default::default()
    };

    let recognizer = OfflineRecognizer::create(&config).expect("failed to create recognizer");

    let wave = Wave::read(wav_path).expect("failed to read wav");
    println!(
        "read {} samples at {} Hz from {}",
        wave.num_samples(),
        wave.sample_rate(),
        wav_path
    );

    let stream = recognizer.create_stream();
    stream.accept_waveform(wave.sample_rate(), wave.samples());
    recognizer.decode(&stream);

    match stream.get_result() {
        Some(result) => {
            println!("raw:   {}", result.text);
            if let Some(punct_path) = punct_model {
                let punctuator =
                    Punctuator::load(Path::new(punct_path)).expect("failed to load punctuator");
                println!("punct: {}", punctuator.punctuate(&result.text));
            }
        }
        None => {
            eprintln!("no recognition result");
            std::process::exit(1);
        }
    }
}
