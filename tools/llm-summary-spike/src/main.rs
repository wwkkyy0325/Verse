//! A measurement, not a feature.
//!
//! Answers one question for `docs/llmwiki/tasks/llm-summary.md`: can a model
//! this small write a summary of a real transcript, on this machine, in a time
//! somebody would wait for?
//!
//!     llm-summary-spike <model.gguf> <tokenizer.json> <transcript.txt> [max new tokens]
//!
//! Deliberately outside the workspace. If the numbers say yes, the thing that
//! ships is a crate of its own with tests and a place in the catalogue; if they
//! say no, this directory is deleted and the numbers go in the changelog.

use std::io::Write as _;
use std::time::Instant;

use candle_core::quantized::gguf_file;
use candle_core::{DType, Device, Tensor};
use candle_transformers::models::quantized_qwen2::ModelWeights;
use tokenizers::Tokenizer;

/// Qwen2.5's chat markers. Written out rather than fetched, because a chat
/// template is how the model was trained and getting it wrong measures the
/// template instead of the model.
const IM_START: &str = "<|im_start|>";
const IM_END: &str = "<|im_end|>";

const SYSTEM: &str = "你是一个会议记录助手。用中文写一段简明的总结，说明这段对话在讲什么、\
有哪些结论或待办。只写总结本身，不要复述原文，不要逐条罗列。";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: llm-summary-spike <model.gguf> <tokenizer.json> <transcript.txt> [max_new]");
        std::process::exit(2);
    }
    let max_new: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(512);

    let device = Device::Cpu;

    // Named in the output, because a measurement that does not say what it
    // measured is not one.
    let model_name = std::path::Path::new(&args[1])
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    let load = Instant::now();
    let mut file = std::fs::File::open(&args[1])?;
    let content = gguf_file::Content::read(&mut file)?;
    let mut model = ModelWeights::from_gguf(content, &mut file, &device)?;
    let tokenizer = Tokenizer::from_file(&args[2]).map_err(|e| e.to_string())?;
    let load_time = load.elapsed();

    let transcript = std::fs::read_to_string(&args[3])?;

    let prompt = format!(
        "{IM_START}system\n{SYSTEM}{IM_END}\n\
         {IM_START}user\n{transcript}{IM_END}\n\
         {IM_START}assistant\n"
    );

    let encoding = tokenizer.encode(prompt.as_str(), true).map_err(|e| e.to_string())?;
    let mut tokens: Vec<u32> = encoding.get_ids().to_vec();
    let prompt_tokens = tokens.len();

    // `<|im_end|>` is how this family ends a turn. Read from the tokenizer so a
    // different one would not silently never stop.
    let eos = tokenizer
        .token_to_id(IM_END)
        .ok_or("the tokenizer does not know <|im_end|>")?;

    println!("model       : {model_name}");
    println!("load        : {:.2?}", load_time);
    println!("transcript  : {} characters", transcript.chars().count());
    println!("prompt      : {prompt_tokens} tokens");
    println!("max new     : {max_new}");
    println!();

    // The prompt in one pass, so the prefill is measured as a prefill.
    let prefill = Instant::now();
    let input = Tensor::new(tokens.as_slice(), &device)?.unsqueeze(0)?;
    let logits = model.forward(&input, 0)?;
    let prefill_time = prefill.elapsed();

    let mut index_pos = prompt_tokens;
    let mut generated = Vec::new();
    let mut first_token = None;
    let mut logits = logits.squeeze(0)?;

    // A presence penalty, because plain greedy decoding is not how this model
    // is meant to be run and it loops: the first attempt produced the same
    // sentence seventeen times. Measuring quality with a decoder that the
    // runtime would never use would measure the decoder.
    const PENALTY: f32 = 2.0;
    let mut said: std::collections::HashSet<u32> = std::collections::HashSet::new();

    let start = Instant::now();
    for _ in 0..max_new {
        if !said.is_empty() {
            let mut scores = logits.to_vec1::<f32>()?;
            for &token in said.iter() {
                scores[token as usize] -= PENALTY;
            }
            logits = Tensor::new(scores.as_slice(), &device)?;
        }

        let next = logits.argmax(candle_core::D::Minus1)?.to_scalar::<u32>()?;
        first_token.get_or_insert(start.elapsed());

        if next == eos {
            break;
        }
        generated.push(next);
        tokens.push(next);
        said.insert(next);

        // A dot per token on stderr, so a long generation is visibly alive
        // without the text being interleaved with progress.
        eprint!(".");
        let _ = std::io::stderr().flush();

        let input = Tensor::new(&[next], &device)?.unsqueeze(0)?;
        logits = model.forward(&input, index_pos)?.squeeze(0)?;
        index_pos += 1;
    }
    eprintln!();

    let decode_time = Instant::now();
    let text = tokenizer.decode(&generated, true).map_err(|e| e.to_string())?;
    let text = text.trim().to_string();

    println!();
    println!("prefill     : {prefill_time:.2?} for {prompt_tokens} tokens");
    println!(
        "first token : {:.2?}",
        first_token.unwrap_or_default()
    );
    println!(
        "generation  : {:.2?} for {} tokens ({:.1} tok/s)",
        start.elapsed(),
        generated.len(),
        generated.len() as f64 / start.elapsed().as_secs_f64().max(1e-9)
    );
    println!("decode      : {:.2?}", decode_time.elapsed());
    println!("total       : {:.2?}", load_time + prefill_time + start.elapsed());
    println!();
    println!("--- summary ---");
    println!("{text}");
    println!("--- end ---");

    // Held so the type parameter is used; candle's device is not Copy.
    let _ = DType::F32;

    Ok(())
}
