//! Generate OpenAPI specification for the Chronoscope API.
//!
//! Usage:
//!   cargo run --bin openapi > openapi.json
//!   cargo run --bin openapi -- output.json

use std::env;

use dropshot::ApiDescription;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut api = ApiDescription::new();
    chronoscope_api::register_api(&mut api)?;

    let openapi = api.openapi("Chronoscope API", semver::Version::new(0, 1, 0));
    let spec_json = serde_json::to_string_pretty(&openapi.json()?)?;

    // Write to file if path provided, otherwise stdout
    if let Some(path) = env::args().nth(1) {
        std::fs::write(&path, &spec_json)?;
        eprintln!("OpenAPI spec written to {path}");
    } else {
        println!("{spec_json}");
    }

    Ok(())
}
