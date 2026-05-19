---
paths:
  - "**/*.rs"
---
# Don't silence clippy lints

The workspace denies `clippy::unwrap_used`, `expect_used`, `panic`, `unreachable`, `todo`, `unimplemented`, plus `unsafe_code`. These catch real bugs. **We don't allow these even in tests** — tests use the same error-handling discipline as production code.

**Do not add `#[allow(clippy::...)]` or `#[expect(clippy::...)]` to make a lint pass.** Fix the underlying issue — propagate with `?`, return `Result`, make the match exhaustive, etc.

If a lint really is wrong at one specific site (rare), use `#[expect(clippy::foo, reason = "concrete justification")]` and surface the override at the top of your work summary so it can be reviewed.
