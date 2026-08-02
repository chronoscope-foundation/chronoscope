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
    let mut problems: Vec<String> = Vec::new();

    for article in ARTICLES {
        let stem = article.stem;
        let (html, page_problems) = render_article(article.markdown);
        problems.extend(
            page_problems
                .into_iter()
                .map(|problem| format!("{stem}.md: {problem}")),
        );
        write_artifact(
            &out_dir.join(output_file(stem)),
            &format!("the rendered body of {stem}.md"),
            &html,
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
    let (faq_source, faq_problems) = generated_faq_source(FAQ_MARKDOWN);
    problems.extend(
        faq_problems
            .into_iter()
            .map(|problem| format!("faq.md: {problem}")),
    );
    write_artifact(
        &out_dir.join("faq.rs"),
        "the generated FAQ table",
        &faq_source,
    )?;

    // Anything the renderers had no place for is content an author wrote that
    // no reader would ever see, so the build stops instead of publishing the
    // page without it. Cargo aborts once a `cargo::error=` line is emitted;
    // returning `Err` as well would bury these behind an escaped blob.
    for problem in &problems {
        println!("cargo::error={problem}");
    }

    println!("cargo:rerun-if-changed=content");
    Ok(())
}
