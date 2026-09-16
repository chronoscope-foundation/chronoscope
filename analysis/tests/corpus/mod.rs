//! Loading corpus images for the model tests that read real pictures. Its own
//! module, not `common`, since only the corpus-reading binaries include it: a
//! helper in `common` would read as dead code in the binaries that don't.

use std::{error, path::Path};

use image::DynamicImage;

/// The env var naming the corpus link farm the `analysis` shell sets, keyed by
/// bare entry id with no file extension.
const CORPUS_ENV: &str = "CORPUS_IMAGES";

/// The corpus link farm directory, from [`CORPUS_ENV`], or a named actionable
/// error if it is unset.
pub fn corpus_dir() -> Result<std::path::PathBuf, String> {
    std::env::var_os(CORPUS_ENV)
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .ok_or_else(|| {
            format!(
                "{CORPUS_ENV} is unset. It names the corpus link farm the `analysis` shell \
                 exports; enter that shell (or run `just model-test`) so the corpus images resolve."
            )
        })
}

/// Loads a corpus single by its bare id. The files carry no extension, so the
/// format is sniffed from the bytes rather than guessed from the path.
pub fn load_corpus(dir: &Path, id: &str) -> Result<DynamicImage, Box<dyn error::Error>> {
    let path = dir.join(id);
    let reader = image::ImageReader::open(&path)
        .map_err(|e| format!("could not open corpus image {}: {e}", path.display()))?
        .with_guessed_format()
        .map_err(|e| format!("could not sniff the format of {}: {e}", path.display()))?;
    let image = reader
        .decode()
        .map_err(|e| format!("could not decode {}: {e}", path.display()))?;
    Ok(image)
}
