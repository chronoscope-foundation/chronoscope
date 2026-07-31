//! Renders the static content pages once, here, rather than shipping a
//! markdown parser to every visitor: `about.md` and `related-work.md` become
//! HTML, and `faq.md` becomes the `static FAQ` table `src/content.rs` includes.

use std::path::Path;

include!("build/render.rs");

/// Write one rendered artifact, saying what was being written and where.
///
/// A build script failure reaches the developer as the `Display` of whatever it
/// returned, under `failed to run custom build command`. A bare `Permission
/// denied` there names neither the artifact nor the path it was going to.
fn write_artifact(
    path: &Path,
    what: &str,
    contents: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::write(path, contents)
        .map_err(|err| format!("writing {what} to {}: {err}", path.display()).into())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = std::env::var("OUT_DIR")?;
    let out_dir = Path::new(&out_dir);

    for article in ARTICLES {
        write_artifact(
            &out_dir.join(output_file(article.stem)),
            &format!("the rendered body of {}.md", article.stem),
            &render_article(article.markdown),
        )?;
    }
    write_artifact(
        &out_dir.join("articles.rs"),
        "the generated article constants and the route macro over them",
        &format!(
            "{}{}",
            generated_articles_source(),
            generated_route_macro_source()
        ),
    )?;
    write_artifact(
        &out_dir.join("faq.rs"),
        "the generated FAQ table",
        &generated_faq_source(FAQ_MARKDOWN),
    )?;

    println!("cargo:rerun-if-changed=content");
    Ok(())
}
