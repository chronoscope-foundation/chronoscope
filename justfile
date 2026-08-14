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
#                            model-test, openapi, infra-plan). Each is
#                            safe-by-construction so it can be allowlisted in
#                            .claude/settings.json without opening a permission
#                            hole the way `nix develop` would.
#
#   `deploy`, `deploy-web`, `infra-apply`
#                          — the exceptions to that: the first two publish an
#                            artifact and roll what production serves, the third
#                            creates and destroys real cloud resources. All
#                            three stay recipes a human types. Allowlisting them
#                            hands that away.
#
# Targets, where supported:
#   all              everything (default)
#   nix              .nix files only
#   rust             native cargo workspace (default-members)
#   web              chronoscope-web (wasm32 target)
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
        nix)          echo "default" ;;
        rust|api|core|db|api-client|ingestion|workers|dev|integrations)
                      echo "api" ;;
        *) echo "unknown target: $1" >&2
           echo "valid: all, nix, rust, web, analysis, api, core, db, api-client, ingestion, workers, dev, integrations" >&2
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

# Resolve the bundle the Worker serves. Built rather than carried forward the
# way the image is: the Cloudflare provider reads this directory while planning
# — it hashes every file to work out what to upload — so it has to be a path
# that exists now, and one recovered from an old state can have been collected.
# The bundle is a pure function of the tree, so building it is also the shortest
# statement of what this tree serves. The --out-link pins it against collection
# between the build and the apply.
_infra_web_dist := '''
_infra_web_dist() {
    if [ -n "${TF_VAR_web_dist:-}" ]; then return 0; fi
    mkdir -p .nix-gc-roots
    TF_VAR_web_dist=$(nix build .#web --out-link .nix-gc-roots/web --print-out-paths)
    export TF_VAR_web_dist
}
'''

# Keep stdin intact for the confirmation an apply asks for at the end.
#
# The builds and pushes in between are free to read stdin, and nix does: a cold
# run left it at EOF, which tofu takes as a refusal, so a first deploy failed at
# the prompt while the cached re-run sailed past. Redirecting each build would
# fix it until someone adds the next one, so stdin is saved once here and
# replaced with /dev/null. Nothing downstream can consume what it cannot reach.
#
# The apply reads the saved copy with `<&3`: a terminal when a human types this,
# a pipe when something scripts it. Reading /dev/tty instead would also fix the
# EOF, at the cost of refusing to run anywhere without a controlling terminal.
_hold_stdin := '''
_hold_stdin() {
    exec 3<&0
    exec 0</dev/null
}
'''

# Load the Cloudflare API token the provider reads, from the login keychain
# unless CLOUDFLARE_API_TOKEN is already set (CI).
_cloudflare_token := '''
_cloudflare_token() {
    if [ -n "${CLOUDFLARE_API_TOKEN:-}" ]; then return 0; fi
    CLOUDFLARE_API_TOKEN="$(security find-generic-password -s chronoscope-cloudflare-token -w 2>/dev/null)" || true
    if [ -z "${CLOUDFLARE_API_TOKEN:-}" ]; then
        echo "No Cloudflare token. Set CLOUDFLARE_API_TOKEN, or store the infra-runner token in the login keychain:" >&2
        echo "  security add-generic-password -U -a \"\$USER\" -s chronoscope-cloudflare-token -w \"\$(pbpaste)\"" >&2
        return 1
    fi
    export CLOUDFLARE_API_TOKEN
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
            nix build \
                ".#checks.$SYS.nix-lint" \
                ".#checks.$SYS.python-scripts-lint" \
                ".#checks.$SYS.python-scripts-typecheck" \
                --no-link
            ;;
        rust)
            # The two Postgres suites each spin an ephemeral cluster, so they
            # cost more than the rest; they belong here anyway, because they are
            # the only place the Postgres backend and the server built on it run
            # against a database rather than only type-check.
            nix build \
                ".#checks.$SYS.fmt" \
                ".#checks.$SYS.clippy" \
                ".#checks.$SYS.doc" \
                ".#checks.$SYS.doctest" \
                ".#checks.$SYS.llvm-cov" \
                ".#checks.$SYS.postgres-smoke" \
                ".#checks.$SYS.api-postgres" \
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
            echo "valid: all, nix, rust, web, linux" >&2
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
            ;;
        rust)
            cargo test
            ;;
        web)
            cargo test -p chronoscope-web
            cargo test -p chronoscope-dev --test web --features chronoscope-dev/browser-tests -- --test-threads=4
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
# Both recipes authenticate to Google as you, through application-default
# credentials:
#   gcloud auth application-default login
#
# Cloudflare authenticates with an API token instead, loaded per run from the
# login keychain (or CLOUDFLARE_API_TOKEN if already set).
#
# An apply prints its plan and waits for a typed confirmation before it touches
# anything.
#
# Two things cannot declare themselves. The state bucket, since the state
# describing it would have to live in it; and the Cloudflare token, since it is
# the credential an apply authenticates with. Create the bucket once, by hand:
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
    {{ _infra_web_dist }}
    {{ _cloudflare_token }}
    _ensure_nix
    # Re-exec on the tool rather than on IN_NIX_SHELL: every dev shell sets that
    # variable and only this one carries tofu.
    if ! command -v tofu >/dev/null 2>&1; then
        exec nix develop .#infra --command just infra-plan
    fi
    _infra_sync
    _infra_image
    _infra_web_dist
    _cloudflare_token
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
    {{ _infra_web_dist }}
    {{ _cloudflare_token }}
    {{ _hold_stdin }}
    _ensure_nix
    if ! command -v tofu >/dev/null 2>&1; then
        exec nix develop .#infra --command just infra-apply
    fi
    _hold_stdin
    _infra_sync
    _infra_image
    _infra_web_dist
    _cloudflare_token
    if [ -z "$TF_VAR_image" ]; then
        # Applying a placeholder would create a service that cannot pull, so
        # send the first run through the recipe that publishes an image.
        echo "error: no image published yet. 'just deploy' builds one, pushes it," >&2
        echo "and applies everything here with the digest the push reported." >&2
        exit 1
    fi
    tofu -chdir=.infra apply <&3

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
    {{ _infra_web_dist }}
    {{ _cloudflare_token }}
    {{ _hold_stdin }}
    _ensure_nix
    # Re-exec on the tools rather than on IN_NIX_SHELL: every dev shell sets
    # that variable and only this one carries all three, so keying off it
    # strands a direnv'd caller after the multi-minute image build.
    if ! command -v gcloud >/dev/null 2>&1 || ! command -v skopeo >/dev/null 2>&1 \
       || ! command -v tofu >/dev/null 2>&1; then
        exec nix develop .#deploy --command just deploy
    fi
    _hold_stdin
    # The same definition the infrastructure is declared from, so the push
    # cannot address a registry nothing ever created.
    project=$(nix eval --file nix/infra-settings.nix project --raw)
    region=$(nix eval --file nix/infra-settings.nix region --raw)
    repo=$(nix eval --file nix/infra-settings.nix artifactRepository --raw)
    service=$(nix eval --file nix/infra-settings.nix cloudRunService --raw)
    # Resolved before the image is built, so a bundle that will not build or a
    # credential that cannot be read stops the deploy before anything is
    # published rather than between the push and the apply.
    _infra_web_dist
    _cloudflare_token
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
    tofu -chdir=.infra apply <&3

# ---------------------------------------------------------------------------
# Deployment to the Cloudflare edge.
#
# The Worker in front of chronoscope.io serves the web bundle as static assets
# and proxies /api to Cloud Run. Assets that match a file in the bundle are
# answered without the script running at all, so the frontend costs no
# invocation and the two halves share one origin: no CORS in the app, and one
# hostname for a passkey to be bound to.
#
# The same shape as the image deploy above. The Worker's shape is declared
# infrastructure (nix/infra.nix) and the bundle arrives as a variable, so this
# recipe builds `packages.web`, names the store path it produced, and lets the
# one tool that owns the Worker publish it. Terraform uploads the directory
# itself, hashing each file to send only what changed, which is why there is no
# separate wrangler step and no second place the Cloud Run URL is written down.
#
# Everything here is one state and one apply, so a deploy of either half plans
# the other. That is deliberate: the checked-in declaration describes the whole
# front of the system, and drift in either half shows up whichever one you roll.
# ---------------------------------------------------------------------------

# Build the web bundle and publish it to the Cloudflare Worker that fronts the site.
deploy-web:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    {{ _infra_sync }}
    {{ _infra_image }}
    {{ _infra_web_dist }}
    {{ _cloudflare_token }}
    {{ _hold_stdin }}
    _ensure_nix
    if ! command -v tofu >/dev/null 2>&1 || ! command -v gcloud >/dev/null 2>&1; then
        exec nix develop .#infra --command just deploy-web
    fi
    _hold_stdin
    _infra_web_dist
    echo "==> Publishing $TF_VAR_web_dist"
    _infra_sync
    _infra_image
    _cloudflare_token
    if [ -z "$TF_VAR_image" ]; then
        # The Worker's API_ORIGIN reads the Cloud Run service's URL, so there is
        # nothing to point the proxy half at until that service exists.
        echo "error: no image published yet. 'just deploy' builds one, pushes it," >&2
        echo "and applies everything here with the digest the push reported." >&2
        exit 1
    fi
    tofu -chdir=.infra apply <&3

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
    nix build .#dinov3-weights --impure --out-link .nix-gc-roots/dinov3-weights
    echo "==> Fetching SAM3 weights..."
    nix build .#sam3-weights --impure --out-link .nix-gc-roots/sam3-weights
    # Qwen is ungated (apache-2.0): no HF_TOKEN, no --impure. ~67 GiB, so it is
    # the long pole here. The `qwen-vlm-uqff` derivation prequantizes it to AFQ4.
    echo "==> Fetching Qwen VLM weights (~67 GiB)..."
    nix build .#qwen-vlm-weights --out-link .nix-gc-roots/qwen-vlm-weights
    echo "Done. Weights pinned as GC roots; the exports rebuild from them."

# Build the ONNX exports the analysis crate loads, and pin them as GC roots.
# Downstream of the weight FODs, so `fetch-weights` (and its HF_TOKEN
# requirement) comes first. The export itself is pure: no token, no network.
#
# DINOv3's resolution sets the patch grid masked pooling reads from; see
# `packages.dinov3-onnx-*` for the variants that exist.
fetch-models dinov3_resolution="224": fetch-weights
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    # Fail before the long export rather than with a bare "attribute not found".
    case "{{ dinov3_resolution }}" in
        224|448) ;;
        *) echo "unknown dinov3 resolution: {{ dinov3_resolution }}" >&2
           echo "valid: 224, 448 (add a variant in flake.nix to extend)" >&2
           exit 1 ;;
    esac
    echo "==> Exporting models to ONNX (several minutes on first build)..."
    # --out-link pins the GC root; --print-out-paths reports the store path the
    # crate should read. Never read .nix-gc-roots/ itself — it is a keep-alive,
    # not a dependency handle.
    sam3_onnx="$(nix build .#sam3-onnx \
        --out-link .nix-gc-roots/sam3-onnx --print-out-paths)"
    # One root per resolution: a shared name would move off the previous export
    # and let the next `nix store gc` reclaim a multi-minute build that the
    # printed DINOV3_ONNX_DIR still points at.
    dinov3_onnx="$(nix build .#dinov3-onnx-{{ dinov3_resolution }} \
        --out-link .nix-gc-roots/dinov3-onnx-{{ dinov3_resolution }} \
        --print-out-paths)"
    echo "SAM3_ONNX_DIR=$sam3_onnx"
    echo "DINOV3_ONNX_DIR=$dinov3_onnx"

# Fetch corpus images from external URLs.
fetch-corpus:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    echo "==> Fetching corpus images..."
    nix build .#corpus-images --no-link
    echo "Done. Corpus images will be pinned as GC roots on next analysis shell entry."

# Fetch the network-dependent inputs: model weights + corpus images.
# Single nix build so all FODs fetch in parallel (different hosts —
# HF, corpus URLs — so concurrency is a clean win).
#
# The ONNX exports are downstream of these and cost minutes of compute rather
# than bandwidth, so they live in `fetch-models` and are not swept in here.
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
    # Everything is realized now, so these cost a store lookup. Each recipe
    # owns the GC root for what it fetches; delegating keeps that ownership in
    # one place, so a weight added there can't come back unrooted here.
    just fetch-weights
    just fetch-corpus

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
# Corpus tooling. The images are the development set the analysis pipeline runs
# against; the fetcher lives in chronoscope-analysis behind the `corpus` feature
# because it only ever runs from Nix.
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
    cargo run --features corpus -p chronoscope-analysis --bin corpus-fetch -- hash

# ---------------------------------------------------------------------------
# Model-dependent tests. Cordoned out of `just check` for the reason `check
# linux` is: they need inputs not every contributor can obtain — a
# multi-hundred-MB export hanging off HF-token weight FODs. The tests are
# `#[ignore]`d rather than feature-gated, so they compile in every build and
# their count stays visible in the ordinary test output.
# ---------------------------------------------------------------------------

# Run the tests that load a real model, against freshly realized artifacts.
model-test:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ _ensure_nix }}
    _ensure_nix
    if [ -z "${IN_NIX_SHELL:-}" ]; then
        exec nix develop .#analysis --command just model-test
    fi
    # Realized here rather than read from the environment, so a test whose
    # artifact is missing fails naming this recipe instead of skipping green.
    # --out-link pins the GC root; --print-out-paths gives the path the tests
    # read. Never read .nix-gc-roots/ itself — it is a keep-alive, not a handle.
    #
    # Each fixture names its own export and holds it in its closure, so
    # realizing the fixtures realizes the graphs they describe.
    mkdir -p .nix-gc-roots
    fixtures=""
    for resolution in 224 448; do
        fixture="$(nix build ".#dinov3-fixture-$resolution" \
            --out-link ".nix-gc-roots/dinov3-fixture-$resolution" \
            --print-out-paths)"
        fixtures="${fixtures:+$fixtures:}$fixture"
    done
    export DINOV3_FIXTURES="$fixtures"
    # The SAM 3 fixture holds its export in its closure, so this realizes the
    # interactive graph the comparison runs against too.
    export SAM3_FIXTURE="$(nix build .#sam3-fixture \
        --out-link .nix-gc-roots/sam3-fixture --print-out-paths)"
    # Qwen 3.6's prequantized AFQ4 UQFF, realized on demand: its derivation
    # quantizes the base BF16 weights on CPU, so the test loads the four-bit
    # shards with the fast Metal kernel instead of running an ISQ pass. The build
    # realizes and pins the dir; `firstShard` names the load target, so the shard
    # filename lives only in the derivation.
    nix build .#qwen-vlm-uqff --out-link .nix-gc-roots/qwen-vlm-uqff
    export QWEN_MODEL_FIRST_SHARD="$(nix eval --raw .#qwen-vlm-uqff.firstShard)"
    cargo test -p chronoscope-analysis -- --ignored
