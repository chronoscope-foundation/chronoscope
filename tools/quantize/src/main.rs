//! Pre-quantizes Qwen 3.6 to an AFQ4 UQFF for the fast Metal load path.
//!
//! `qwen-quantize <base-model-dir> <output-dir>`
//!
//! Loads the base BF16 safetensors and writes a self-contained AFQ4 UQFF into
//! the output directory: sharded `afq4-N.uqff`, `residual.safetensors`, and the
//! config/tokenizer/preprocessor files the loader reads back. mistral.rs forces
//! the write onto CPU, so no GPU is involved; the runtime then loads the
//! four-bit shards with the Metal `MoE` kernel and skips the ISQ pass. Invoked
//! only by the `qwen-vlm-uqff` Nix derivation, which contract-checks the output.

use std::path::Path;
use std::process::ExitCode;

use mistralrs::{IsqType, MultimodalModelBuilder};

/// Shard basename handed to mistral.rs; it writes `<stem>-N.uqff` beside the
/// residual and config. The loader discovers the shard by extension, so this
/// name has no reader and lives only here.
const AFQ4_STEM: &str = "afq4";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let [_, base_dir, out_dir] = args.as_slice() else {
        eprintln!("usage: qwen-quantize <base-model-dir> <output-dir>");
        return ExitCode::FAILURE;
    };
    match run(base_dir, out_dir) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("qwen-quantize: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(base_dir: &str, out_dir: &str) -> Result<(), String> {
    let output = Path::new(out_dir).join(format!("{AFQ4_STEM}.uqff"));
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|source| format!("could not start the tokio runtime: {source}"))?;
    runtime.block_on(async {
        MultimodalModelBuilder::new(base_dir)
            .with_logging()
            .with_isq(IsqType::AFQ4)
            .write_uqff(output)
            .build()
            .await
            .map_err(|source| format!("{source:?}"))?;
        Ok(())
    })
}
