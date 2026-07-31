//! Runner for the build script's render suite: `cargo test` does not run a
//! build script's own `#[test]`s, so the file is pulled in here as well.
//!
//! Declared as a module so `cargo fmt` reaches `web/build/render.rs`: rustfmt
//! follows module declarations and not `include!`, and that is what puts the
//! file under the gate's format check.
#[path = "../build/render.rs"]
mod render;
