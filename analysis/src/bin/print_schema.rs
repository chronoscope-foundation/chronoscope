use chronoscope_analysis::schema::{AnalysisResult, VlmAnalysis};

fn main() -> Result<(), serde_json::Error> {
    let args: Vec<String> = std::env::args().collect();
    let schema_type = args.get(1).map(|s| s.as_str()).unwrap_or("vlm");

    match schema_type {
        "vlm" => {
            let schema = schemars::schema_for!(VlmAnalysis);
            println!("{}", serde_json::to_string_pretty(&schema)?);
        }
        "result" => {
            let schema = schemars::schema_for!(AnalysisResult);
            println!("{}", serde_json::to_string_pretty(&schema)?);
        }
        _ => {
            eprintln!("Usage: print_schema [vlm|result]");
            eprintln!("  vlm    - VLM output schema (default)");
            eprintln!("  result - Full AnalysisResult schema");
            std::process::exit(1);
        }
    }
    Ok(())
}
