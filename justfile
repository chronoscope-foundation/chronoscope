# Chronoscope dev workflow.
#
# Three layers of recipe:
#
#   `just check [target]`  — hermetic ground truth via `nix flake check`
#                            (or a focused subset). Writes .claude/last-check.json
#                            on success; the pre-commit hook keys off this marker.
#
#   `just fmt|test|clippy [target]`
#                          — fast inner loop. Cargo direct in the right dev shell.
#                            Incremental compilation against ./target. Does NOT
#                            satisfy the pre-commit gate — only `just check` does.
#
#   Everything else        — concrete actions (web-dev, fetch-*, corpus-*,
#                            openapi, infra-plan). Each is safe-by-construction
#                            so it can be allowlisted in .claude/settings.json
#                            without opening a permission hole the way
#                            `nix develop` would.
#
#   `deploy`, `infra-apply`
#                          — the exceptions to that: one publishes an image and
#                            rolls the production service, the other creates and
#                            destroys real cloud resources. Both stay recipes a
#                            human types. Allowlisting them hands that away.
#
# Targets, where supported:
#   all              everything (default)
#   nix              .nix files only
#   rust             native cargo workspace (default-members)
#   web              chronoscope-web (wasm32 target)
#   triton           analysis/triton/ python
#   linux            `check` only: the x86_64-linux checks (needs a builder
#                    for that system; outside the commit gate)
#   analysis         chronoscope-analysis crate
#   <crate-name>     single crate (core, db, api, api-client, ingestion,
#                                  workers, dev, integrations)

# ---------------------------------------------------------------------------
# Shared bash helpers (substituted into each recipe via `{{ _name }}`).
# ---------------------------------------------------------------------------

_ensure_nix := '''
_ensure_nix() {
    if ! command -v nix &>/dev/null; then
        . /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh 2>/dev/null \
        || . /nix/var/nix/profiles/default/etc/profile.d/nix.sh 2>/dev/null \
        || { echo "error: nix not found" >&2; exit 1; }
    fi
}
'''

# Map a target name to the dev shell that has the right deps. Used by
# fmt/test/clippy to pick the smallest viable shell — `clippy web` re-execs
# into `web`, not `analysis`, so we don't drag in weights+corpus.
_shell_for_target := '''
_shell_for_target() {
    case "$1" in
        all|analysis) echo "analysis" ;;
        web)          echo "web" ;;
        triton)       echo "triton" ;;
        nix)          echo "default" ;;
        rust|api|core|db|api-client|ingestion|workers|dev|integrations)
                      echo "api" ;;
        *) echo "unknown target: $1" >&2
           echo "valid: all, nix, rust, web, triton, analysis, api, core, db, api-client, ingestion, workers, dev, integrations" >&2
           return 1 ;;
    esac
}
'''

# Re-exec the current recipe inside the right dev shell, if not already in one.
_nix_reexec_for_target := '''
_nix_reexec_for_target() {
    local recipe="$1" target="$2"
    if [ -n "${IN_NIX_SHELL:-}" ]; then return 0; fi
    local shell
    shell=$(_shell_for_target "$target") || exit 1
    exec nix develop ".#$shell" --command just "$recipe" "$target"
}
'''

# Compile the terranix modules and stage them where tofu runs. `.infra/` is
# gitignored working state: the generated config, plus the lock file and
# provider links tofu keeps beside it. The generated file is copied rather than
# symlinked because tofu writes into the directory it reads from.
_infra_sync := '''
_infra_sync() {
    local cfg
    cfg=$(nix build .#infra-config --no-link --print-out-paths)
    mkdir -p .infra
    install -m 644 "$cfg" .infra/config.tf.json
    tofu -chdir=.infra init -input=false
}
'''

# Resolve the image the Cloud Run service should run. `deploy` exports the
# digest it just pushed; everything else carries forward what the last apply
# recorded, so an infrastructure-only change leaves the running revision alone.
# Runs after _infra_sync, which is what initializes the state the output is read
# from.
_infra_image := '''
_infra_image() {
    if [ -n "${TF_VAR_image:-}" ]; then return 0; fi
    TF_VAR_image=$(tofu -chdir=.infra output -raw image 2>/dev/null || true)
    # A state that carries no outputs at all warns and exits 0, with the
    # warning on stdout. An image reference has no whitespace in it, so that is
    # what separates one from anything tofu says instead.
    case "$TF_VAR_image" in
        *[[:space:]]*) TF_VAR_image="" ;;
    esac
    export TF_VAR_image
}
'''

# Build cached analysis-results derivation and pin as GC root.
_build_analysis_results := '''
_build_analysis_results() {
    export ANALYSIS_RESULTS=$(nix build .#analysis-results --no-link --print-out-paths)
    nix-store --realise "$ANALYSIS_RESULTS" --add-root .nix-gc-roots/analysis-results > /dev/null 2>&1 || true
}
'''


# ---------------------------------------------------------------------------
# Ground truth: hermetic checks via `nix flake check` (or focused subset).
# Writes .claude/last-check.json on success.
# ---------------------------------------------------------------------------

# Run hermetic checks for [target] (default: all). On success, writes
# .claude/last-check.json so the pre-commit hook can confirm the gate
# was passed against the current tree state.
check target="all":
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    SYS=$(nix eval --impure --expr 'builtins.currentSystem' --raw)
    # Hashed before the checks read the tree, not after. The marker asserts
    # "this exact tree passed", and hashing at the end would let an edit made
    # during a run — normal enough on a gate this long — be certified by checks
    # that never saw it.
    CHECKED_HASH=$(.claude/hooks/tree-hash.sh)
    case "{{ target }}" in
        all)
            nix flake check
            ;;
        nix)
            nix build ".#checks.$SYS.nix-lint" --no-link
            ;;
        rust)
            nix build \
                ".#checks.$SYS.fmt" \
                ".#checks.$SYS.clippy" \
                ".#checks.$SYS.doc" \
                ".#checks.$SYS.doctest" \
                ".#checks.$SYS.llvm-cov" \
                --no-link
            ;;
        web)
            # web-test (the browser suite) is intentionally absent: it is
            # ordered after the compile/coverage-heavy checks (see nix/rust.nix)
            # so it runs unstarved, which makes it a whole-gate check. Run it via
            # `just check` (full) or iterate with `just test web` in the web shell.
            nix build \
                ".#checks.$SYS.web-build" \
                ".#checks.$SYS.web-test-build" \
                ".#checks.$SYS.web-clippy" \
                ".#checks.$SYS.web-native-test" \
                ".#checks.$SYS.web-native-clippy" \
                --no-link
            ;;
        triton)
            nix build \
                ".#checks.$SYS.triton-fmt" \
                ".#checks.$SYS.triton-lint" \
                ".#checks.$SYS.triton-typecheck" \
                ".#checks.$SYS.triton-test" \
                --no-link
            ;;
        linux)
            # x86_64-linux is what the API deploys onto, and `nix flake check`
            # only evaluates the current system, so from a darwin machine these
            # are asked for by name and need a builder for that system. Outside
            # the commit gate on purpose (see CLAUDE.md): the image boot proves
            # the linked binary starts under the image's own environment, and
            # the suite run is where a filesystem or signal assumption that
            # holds on darwin shows up.
            # -L streams build logs: this run is long and unattended, so
            # without it the suite is a silent block that only becomes
            # diagnosable once it has already finished or failed.
            nix build \
                ".#checks.x86_64-linux.oci-api-boots" \
                ".#checks.x86_64-linux.llvm-cov" \
                --no-link -L
            ;;
        *)
            echo "error: \`just check\` only accepts coarse targets (hermetic)." >&2
            echo "valid: all, nix, rust, web, triton, linux" >&2
            echo "for per-crate iteration, use \`just clippy <crate>\` or \`just test <crate>\`." >&2
            exit 1
            ;;
    esac
    mkdir -p .claude
    if [[ "$CHECKED_HASH" != "$(.claude/hooks/tree-hash.sh)" ]]; then
        echo "note: the tree changed while the checks ran." >&2
        echo "      the marker records the tree that was actually checked, so the" >&2
        echo "      pre-commit hook will prompt until you re-run against this one." >&2
    fi
    cat > .claude/last-check.json <<EOF
    {
      "hash": "$CHECKED_HASH",
      "timestamp": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
      "target": "{{ target }}"
    }
    EOF
    echo "✓ check ({{ target }}) passed; marker written"

# ---------------------------------------------------------------------------
# Fast inner loop: cargo direct, dev shell. NOT a substitute for `just check`.
# ---------------------------------------------------------------------------

# Apply formatting (does not check; use `just check` for that).
fmt target="all":
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    {{ _shell_for_target }}
    {{ _nix_reexec_for_target }}
    _ensure_nix
    _nix_reexec_for_target fmt "{{ target }}"
    case "{{ target }}" in
        all)
            find . -name '*.nix' -not -path './.git/*' -not -path './.direnv/*' -print0 \
              | xargs -0 nixfmt
            cargo fmt
            cargo fmt -p chronoscope-web
            (cd analysis/triton && ruff format .)
            ;;
        nix)
            find . -name '*.nix' -not -path './.git/*' -not -path './.direnv/*' -print0 \
              | xargs -0 nixfmt
            ;;
        rust)
            cargo fmt
            cargo fmt -p chronoscope-web
            ;;
        web)
            cargo fmt -p chronoscope-web
            ;;
        triton)
            cd analysis/triton && ruff format .
            ;;
        *)
            cargo fmt -p chronoscope-{{ target }}
            ;;
    esac

# Run tests (cargo direct, fast incremental). Use `just check` for the
# hermetic version that gates commits.
test target="all":
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    {{ _shell_for_target }}
    {{ _nix_reexec_for_target }}
    _ensure_nix
    _nix_reexec_for_target test "{{ target }}"
    case "{{ target }}" in
        all)
            cargo test
            cargo test -p chronoscope-web
            cargo test -p chronoscope-dev --test web --features chronoscope-dev/browser-tests -- --test-threads=4
            (cd analysis/triton && pytest -v)
            ;;
        rust)
            cargo test
            ;;
        web)
            cargo test -p chronoscope-web
            cargo test -p chronoscope-dev --test web --features chronoscope-dev/browser-tests -- --test-threads=4
            ;;
        triton)
            cd analysis/triton && pytest -v
            ;;
        *)
            cargo test -p chronoscope-{{ target }}
            ;;
    esac

# Run clippy (cargo direct, fast incremental). Use `just check` for the
# hermetic version.
#
# `--all-features` matches the gate's clippy. Without it the inner loop can't
# lint anything behind a feature — which is why `db`'s postgres backend went
# unlinted for its whole life, with no way to notice short of a hand-rolled
# cargo invocation.
clippy target="all":
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    {{ _shell_for_target }}
    {{ _nix_reexec_for_target }}
    _ensure_nix
    _nix_reexec_for_target clippy "{{ target }}"
    # Two passes over chronoscope-web, mirroring the `web-clippy` and
    # `web-native-clippy` flake checks: wasm32 covers what ships, and the host
    # pass is where `--all-targets` reaches the crate's test code.
    # `chronoscope-dev/browser-tests` gates `dev/tests/web.rs` into existence,
    # so clippy only sees that target with the feature on.
    case "{{ target }}" in
        all)
            cargo clippy --all-targets --all-features -- -D warnings
            cargo clippy -p chronoscope-web --target wasm32-unknown-unknown --all-features -- -D warnings
            cargo clippy -p chronoscope-web --all-targets --all-features -- -D warnings
            (cd analysis/triton && ruff check .)
            ;;
        rust)
            cargo clippy --all-targets --all-features -- -D warnings
            ;;
        web)
            cargo clippy -p chronoscope-web --target wasm32-unknown-unknown --all-features -- -D warnings
            cargo clippy -p chronoscope-web --all-targets --all-features -- -D warnings
            ;;
        dev)
            cargo clippy -p chronoscope-dev --all-targets --all-features -- -D warnings
            ;;
        triton)
            cd analysis/triton && ruff check .
            ;;
        *)
            cargo clippy -p chronoscope-{{ target }} --all-targets --all-features -- -D warnings
            ;;
    esac

# ---------------------------------------------------------------------------
# Dev servers and concrete actions.
# ---------------------------------------------------------------------------

# Start the integrated web dev server (API + Trunk live reload) over a
# mounted facts DB (curated|1k|100k|full — realized on demand via Nix,
# served read-only in place). The binary picks
# free ports for API + Trunk automatically — no need to kill anything else
# on common ports.
web-dev subset="curated":
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    if [ -z "${IN_NIX_SHELL:-}" ]; then
        exec nix develop .#web --command just web-dev "{{ subset }}"
    fi
    # Realize the facts DB on demand and use the store path Nix reports; the
    # `--out-link` pins a GC root so it survives collection. Never read from
    # `.nix-gc-roots/` — that symlink is a keep-alive, not a dependency handle.
    mkdir -p .nix-gc-roots
    db="$(nix build ".#wikidata-facts-db-{{ subset }}" \
        --out-link ".nix-gc-roots/wikidata-facts-db-{{ subset }}" --print-out-paths)/facts.db"
    CHRONOSCOPE_FACTS_DB="$db" CHRONOSCOPE_FACTS_DB_SUBSET="{{ subset }}" \
        cargo run -p chronoscope-dev --bin web-dev

# Build the OpenAPI spec (Nix) and report its store path — inspect the contract.
openapi:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    out=$(nix build .#openapi --no-link --print-out-paths)
    echo "openapi spec: $out/openapi.json"

# Regenerate the Xcode project. Nix splices the API-package + Swift-tool store
# paths into the xcodegen spec; pinning the spec keeps that closure alive.
xcodegen:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    if [ -z "${IN_NIX_SHELL:-}" ]; then
        exec nix develop .#ios --command just xcodegen
    fi
    mkdir -p .nix-gc-roots
    spec=$(nix build .#ios-project-spec --out-link .nix-gc-roots/ios-project-spec --print-out-paths)
    cd ios
    xcodegen generate --spec "$spec" --project-root . --project .

# ---------------------------------------------------------------------------
# Infrastructure: terranix modules compiled to OpenTofu config.
#
# nix/infra-settings.nix is the one definition of the project's cloud
# coordinates. The terranix modules declare resources from it and `deploy`
# below reads it back, so an image can only be pushed to a registry that was
# actually declared.
#
# Both recipes authenticate as you, through application-default credentials:
#   gcloud auth application-default login
#
# An apply prints its plan and waits for a typed confirmation before it touches
# anything.
#
# The state bucket is the one thing that cannot declare itself, since the state
# describing it would have to live in it. Create it once, by hand:
#
#   bucket="gs://$(nix eval --file nix/infra-settings.nix stateBucket --raw)"
#   gcloud storage buckets create "$bucket" \
#       --project="$(nix eval --file nix/infra-settings.nix project --raw)" \
#       --location="$(nix eval --file nix/infra-settings.nix region --raw)" \
#       --uniform-bucket-level-access --public-access-prevention
#   gcloud storage buckets update "$bucket" --versioning
#
# Versioning is what makes a truncated or clobbered state write recoverable.
# ---------------------------------------------------------------------------

# Show what OpenTofu would change. Reads live cloud state; writes nothing.
infra-plan:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    {{ _infra_sync }}
    {{ _infra_image }}
    _ensure_nix
    # Re-exec on the tool rather than on IN_NIX_SHELL: every dev shell sets that
    # variable and only this one carries tofu.
    if ! command -v tofu >/dev/null 2>&1; then
        exec nix develop .#infra --command just infra-plan
    fi
    _infra_sync
    _infra_image
    if [ -z "$TF_VAR_image" ]; then
        # A plan writes nothing, so a placeholder here costs nothing and keeps
        # the rest of the plan readable before anything has been published.
        export TF_VAR_image=none-published-yet
        echo "note: no image published yet; planning the service with a placeholder" >&2
    fi
    tofu -chdir=.infra plan

# Apply the OpenTofu config: creates, changes and destroys real cloud resources.
infra-apply:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    {{ _infra_sync }}
    {{ _infra_image }}
    _ensure_nix
    if ! command -v tofu >/dev/null 2>&1; then
        exec nix develop .#infra --command just infra-apply
    fi
    _infra_sync
    _infra_image
    if [ -z "$TF_VAR_image" ]; then
        # Applying a placeholder would create a service that cannot pull, so
        # send the first run through the recipe that publishes an image.
        echo "error: no image published yet. 'just deploy' builds one, pushes it," >&2
        echo "and applies everything here with the digest the push reported." >&2
        exit 1
    fi
    # Read the confirmation off the terminal rather than inherited stdin. A
    # build earlier in the recipe can leave stdin at EOF, and tofu reads that
    # as a refusal, so a cold run fails at the prompt while a warm one works.
    tofu -chdir=.infra apply < /dev/tty

# ---------------------------------------------------------------------------
# Deployment to Cloud Run.
#
# The image is a Nix derivation, so the same tree always produces the same
# bytes; deploying by digest makes the running revision a content hash of the
# tree it came from, and takes a mutable tag out of the path between build and
# production.
#
# The service's shape is declared infrastructure (nix/infra.nix): its memory,
# its environment, its runtime identity, its startup probe, the secret it reads
# and who may invoke it. A deploy builds the image, pushes it, and hands the
# digest to `tofu apply` as a variable, so one tool owns the service and the
# checked-in config keeps describing what is actually running. That last step
# reads the same application-default credentials the infra recipes above do.
#
# Secrets stay out of the image and out of the revision. JWT_SECRET is generated
# on first apply, kept in Secret Manager, and reaches the container as a secret
# reference the runtime service account is allowed to read.
#
# The server answers an unauthenticated readiness probe at GET /health, which
# reports whether both stores opened. The declared startup probe points there,
# so a revision that came up without a usable store never takes traffic; the
# default TCP probe passes as soon as the port is bound.
#
# The instance filesystem is memory-backed and per-instance: the app database
# and the fact-store overlay both live under /tmp and vanish with the instance,
# and their size is charged against the declared memory limit. Nothing written
# through the API survives a revision, which is why that limit is sized for the
# read path plus whatever a session accumulates rather than for a growing store.
#
# The Artifact Registry repo the push targets is declared infrastructure too;
# `just infra-apply` is what creates it, and it has to exist before the push
# below can land.
# ---------------------------------------------------------------------------

# Build the API image, push it to Artifact Registry, apply Cloud Run onto the digest.
deploy:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    {{ _infra_sync }}
    _ensure_nix
    # Re-exec on the tools rather than on IN_NIX_SHELL: every dev shell sets
    # that variable and only this one carries all three, so keying off it
    # strands a direnv'd caller after the multi-minute image build.
    if ! command -v gcloud >/dev/null 2>&1 || ! command -v skopeo >/dev/null 2>&1 \
       || ! command -v tofu >/dev/null 2>&1; then
        exec nix develop .#deploy --command just deploy
    fi
    # The same definition the infrastructure is declared from, so the push
    # cannot address a registry nothing ever created.
    project=$(nix eval --file nix/infra-settings.nix project --raw)
    region=$(nix eval --file nix/infra-settings.nix region --raw)
    repo=$(nix eval --file nix/infra-settings.nix artifactRepository --raw)
    service=$(nix eval --file nix/infra-settings.nix cloudRunService --raw)
    # Cloud Run is linux/amd64; build that system's image regardless of the
    # machine driving the deploy. The --out-link pins the manifest and every
    # layer store path it names, so the push below can't race collection.
    mkdir -p .nix-gc-roots
    image=$(nix build .#packages.x86_64-linux.oci-api \
        --out-link .nix-gc-roots/oci-api --print-out-paths)
    # nix2container tags an untagged image by its manifest derivation's hash,
    # which is the basename of the path the build just printed. Reading it here
    # keeps the tag tied to the artifact in hand.
    tag=${image##*/}
    tag=${tag%%-*}
    ref="$region-docker.pkg.dev/$project/$repo/$service"

    # A short-lived access token in a 0600 file, rather than a credential helper
    # or --dest-creds: nothing depends on ~/.docker state, and the token stays
    # out of the process table.
    authfile=$(mktemp)
    digestfile=$(mktemp)
    trap 'rm -f "$authfile" "$digestfile"' EXIT
    token=$(gcloud auth print-access-token --project "$project")
    basic=$(printf 'oauth2accesstoken:%s' "$token" | base64 | tr -d '\n')
    cat > "$authfile" <<JSON
    { "auths": { "$region-docker.pkg.dev": { "auth": "$basic" } } }
    JSON

    echo "==> Pushing $ref:$tag"
    # --insecure-policy: a nix shell has no /etc/containers/policy.json, and the
    # source here is the local store rather than a registry to verify.
    # --digestfile reports what this push produced, so the deploy names the
    # bytes it just sent instead of resolving a tag someone else could move.
    skopeo --insecure-policy copy --authfile "$authfile" \
        --digestfile "$digestfile" "nix:$image" "docker://$ref:$tag"
    digest=$(cat "$digestfile")

    echo "==> Deploying $ref@$digest"
    # The bytes just pushed, by digest, named to the one tool that owns the
    # service. Everything else about the revision comes from the declaration,
    # so an apply that finds nothing else changed rolls the image and stops
    # there. It prints its plan and waits for a typed confirmation first.
    export TF_VAR_image="$ref@$digest"
    _infra_sync
    # From the terminal, not inherited stdin: the image build above leaves
    # stdin at EOF, which tofu reads as a refusal. That is why a first deploy
    # failed at the prompt and the cached re-run did not.
    tofu -chdir=.infra apply < /dev/tty

# ---------------------------------------------------------------------------
# Data fetches. Each is a thin wrapper around `nix build` + GC root pinning.
# ---------------------------------------------------------------------------

# Fetch model weights (DINOv3 + SAM3) from Hugging Face.
# Requires HF_TOKEN env var on first fetch (the FOD will fail with
# instructions if missing).
fetch-weights:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    echo "==> Fetching DINOv3 weights..."
    nix build .#dinov3-weights --impure --no-link
    echo "==> Fetching SAM3 weights..."
    nix build .#sam3-weights --impure --no-link
    echo "Done. Weights will be pinned as GC roots on next analysis/triton shell entry."

# Fetch corpus images from external URLs.
fetch-corpus:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    echo "==> Fetching corpus images..."
    nix build .#corpus-images --no-link
    echo "Done. Corpus images will be pinned as GC roots on next analysis shell entry."

# Fetch everything: model weights + corpus images.
# Single nix build so all FODs fetch in parallel (different hosts —
# HF, corpus URLs — so concurrency is a clean win).
fetch-all:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    echo "==> Fetching weights, corpus in parallel..."
    nix build \
        .#dinov3-weights \
        .#sam3-weights \
        .#corpus-images \
        --impure --no-link
    echo "Done."

# Build + pin the Wikidata architectural-entities set. Runs the bulk pipeline:
# fetch the ~109GB dump (FOD, on first run), resolve the P279 type set, and
# filter down to architectural entities. Expensive — run rarely. The
# --out-link is itself the GC root, so ordinary shell entry never triggers it.
fetch-wikidata:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    mkdir -p .nix-gc-roots
    echo "==> Building + pinning architectural-entities set (downloads the dump on first run)..."
    nix build .#wikidata-arch-entities --out-link .nix-gc-roots/wikidata-arch-entities
    echo "Done. Pinned at .nix-gc-roots/wikidata-arch-entities/entities.jsonl"

# Build + pin a SQLite facts DB (size: curated | 1k | 100k | full). Every size
# above `curated` needs the architectural-entities set (and thus the dump).
fetch-wikidata-db size="full":
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    mkdir -p .nix-gc-roots
    nix build ".#wikidata-facts-db-{{ size }}" --out-link ".nix-gc-roots/wikidata-facts-db-{{ size }}"
    echo "Done. Pinned at .nix-gc-roots/wikidata-facts-db-{{ size }}/facts.db"

# ---------------------------------------------------------------------------
# Corpus tooling. Live in their own world because the corpus pipeline has
# specific feature flags and a separate analysis-results derivation.
# ---------------------------------------------------------------------------

# Generate / update corpus FOD hashes (fetches new URLs, skips existing).
corpus-hash:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    if [ -z "${IN_NIX_SHELL:-}" ]; then
        exec nix develop .#analysis --command just corpus-hash
    fi
    cargo run --features corpus-test -p chronoscope-analysis --bin corpus-fetch -- hash

# Run the corpus test suite (per-image + per-cluster, with known-issue tracking).
corpus-test:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    {{ _build_analysis_results }}
    _ensure_nix
    if [ -z "${IN_NIX_SHELL:-}" ]; then
        exec nix develop .#analysis --command just corpus-test
    fi
    _build_analysis_results
    cargo test --features corpus-test -p chronoscope-analysis --test corpus_tests

# Run the corpus test suite with VLM (requires remote Triton).
corpus-test-vlm:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    {{ _build_analysis_results }}
    _ensure_nix
    if [ -z "${IN_NIX_SHELL:-}" ]; then
        exec nix develop .#analysis --command just corpus-test-vlm
    fi
    _build_analysis_results
    cargo test --features corpus-test-vlm -p chronoscope-analysis --test corpus_tests
