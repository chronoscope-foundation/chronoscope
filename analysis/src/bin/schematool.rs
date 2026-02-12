use std::io::Read;

use chronoscope_analysis::schema::AnalysisResult;
use chronoscope_analysis::schema::vlm_schema::SubimageOutput;

fn print_schema<T: schemars::JsonSchema>() {
    let schema = schemars::schema_for!(T);
    match serde_json::to_string_pretty(&schema) {
        Ok(s) => println!("{s}"),
        Err(e) => {
            eprintln!("Schema serialization failed: {e}");
            std::process::exit(1);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let schema_type = args.get(1).map(|s| s.as_str()).unwrap_or("vlm");

    match schema_type {
        "vlm" => print_schema::<SubimageOutput>(),
        "result" => print_schema::<AnalysisResult>(),
        "validate" => {
            let mut input = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut input) {
                eprintln!("Failed to read stdin: {e}");
                std::process::exit(1);
            }
            match serde_json::from_str::<AnalysisResult>(&input) {
                Ok(_) => println!("OK"),
                Err(e) => {
                    eprintln!("Validation failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        _ => {
            eprintln!("Usage: schematool [vlm|result|validate]");
            eprintln!("  vlm      - VLM subimage output schema (default)");
            eprintln!("  result   - Full AnalysisResult schema");
            eprintln!("  validate - Read JSON from stdin and validate against AnalysisResult");
            std::process::exit(1);
        }
    }
}
