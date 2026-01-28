use chronoscope_analysis::schema::VlmAnalysis;

fn main() -> Result<(), serde_json::Error> {
    let schema = schemars::schema_for!(VlmAnalysis);
    println!("{}", serde_json::to_string_pretty(&schema)?);
    Ok(())
}
