# AGENTS.md

This file provides guidance for AI coding agents working in the
`infra-controller` repository.

## Project Overview

**NVIDIA Infra Controller (NICo)** is an API-based microservice written in Rust
and Golang that provides site-local, zero-trust, bare-metal lifecycle
management with DPU-enforced isolation. It automates the complexity of the
bare-metal lifecycle to fast-track building next-generation AI Cloud offerings.

> **Status:** Active development. APIs, configurations, and features may
> change without notice between releases.

### Key Responsibilities

- Hardware inventory management and orchestration
- Redfish-based hardware management
- Hardware testing and firmware updates
- IP address allocation and DNS services
- Power control (on/off/reset)
- Provisioning, wiping, and node-release orchestration
- Machine trust enforcement during tenant switching

## Repository Structure

```text
infra-controller/
├── crates/              # Rust crate implementations. To discover all crates
│                        # and their purpose, run `ls crates/` or see the
│                        # [workspace] members list in `Cargo.toml` — each
│                        # crate's own `Cargo.toml` has a `description` field.
│                        # Note: the directory name does NOT always equal the
│                        # crate name (e.g. crates/api/ → crate nico-api).
│                        # Use `grep '^name =' crates/<dir>/Cargo.toml | head -1`
│                        # to get the actual crate name before running
│                        # `cargo test -p <name>` or similar.
├── book/                # mdBook documentation
├── deploy/              # Kubernetes deployment configs and Kustomization overlays
├── dev/                 # Local dev tools (Dockerfiles, test configs, certs)
├── helm/                # Helm chart for Kubernetes deployment
├── bluefield/           # BlueField DPU-specific components
├── pxe/                 # PXE boot artifact generation
├── lints/               # Custom Clippy lints (carbide-lints crate)
├── include/             # Shared Makefile fragments
├── .github/             # GitHub Actions workflows and templates
├── rest-api/            # Golang-based REST API
├── Cargo.toml           # Workspace dependency management
├── Makefile.toml        # Primary build/task automation
├── Makefile-build.toml  # Build-specific tasks
└── Makefile-package.toml # Packaging tasks
```

## Technology Stack

### gRPC API and components

- **Language:** Rust (edition 2024, toolchain pinned in `rust-toolchain.toml`)
- **Async runtime:** Tokio
- **gRPC framework:** Tonic (with TLS via Rustls/aws_lc_rs)
- **HTTP framework:** Axum (pinned; see `Cargo.toml` for compatibility rationale)
- **Database:** SQLx (compile-time checked queries)
- **Observability:** OpenTelemetry, Tracing (structured logfmt logging)
- **Build tool:** `cargo-make` (TOML task runner)
- **API definitions:** Protocol Buffers (protobuf)

### REST API and components

- **Language (REST API):** Golang 1.26.x

## Build, Test, and Lint Commands

### REST API contract conventions

- Do not use `omitempty` on REST API response fields. Clients must be able to
  distinguish an empty value from a field unsupported by the API version.
- Paginated operations must implement deterministic ordering before pagination
  and document every supported `orderBy` value and its default in OpenAPI. Do
  not rely on an upstream API or database's implicit result order.

All task automation uses `cargo-make`. Install it with:

```bash
cargo install cargo-make
```

### Building

```bash
# Standard debug build (all workspace crates)
cargo build

# Release build
cargo build --release

# Full CI build + test (mirrors what CI runs)
cargo make build-and-test-release-container-services

# Build the admin CLI locally
cargo make build-cli
```

### Testing

```bash
# Run all tests
cargo test

# Build prerequisites first, then test (recommended for integration tests)
cargo make correctly-execute-tests
```

When writing tests, prefer the **table-driven** style and helpers from
`carbide-test-support`; use the [Testing section in `STYLE_GUIDE.md`](STYLE_GUIDE.md#testing)
for table structure and API details. Use grouped `scenarios!` / `value_scenarios!`
or explicit `check_cases` / `check_values` when cases share one operation and
assertion form. When cases share setup but require different assertions, use a
local case table that keeps each case's check next to its inputs.

Before adding coverage, inventory the relevant unit, database, controller, and
integration tests. Each new test should have one reason to exist: an observable
contract or distinct failure boundary that no retained test protects. Use the
smallest set of cases that exercise different behavior. Do not enumerate a
Cartesian product merely because inputs are booleans or enums; enumerate a
combination only when it is reachable and protects distinct observable behavior
or a distinct failure boundary, including precedence between conflicting inputs.

Place each proof at the narrowest layer that can exercise the contract.
Higher-level tests should prove wiring, persistence, transaction behavior,
concurrency, or external effects that lower-level tests cannot; do not repeat a
lower-level case matrix at higher layers. For every new test or row, ask:
**What regression does this catch that no retained test catches?** If there is
no concrete answer, merge it into existing coverage or delete it.

`STYLE_GUIDE.md` remains the source for helper APIs and table layout. For
changes written by agents, the rules above for choosing cases replace its
recommendation to enumerate every branch and input variant.

For state-machine branch tests, reload persisted state after the controller
iteration and assert the branch-owned fields or counters. An unchanged visible
state or absence of an external action does not prove which branch ran.
For retry tests, inject the claimed transient failure and assert that a later
iteration retries it while preserving the expected externally visible state. A
simulator's default unsupported response does not prove transient recovery.

For user-visible CLI table changes, exercise the public command in a test and
assert the rendered headers plus populated and empty cell values. Helper-only
tests do not prove the table contract.

Keep test rack-profile capability counts aligned with the inventory the fixture
actually instantiates. Use zero for unsupported component types so tests do not
generate expected-but-absent discovery errors.

### Linting and Formatting

```bash
# Run all pre-commit checks (what CI runs)
cargo make pre-commit-verify-workspace

# Individual checks:
cargo make clippy              # Clippy linter (warnings = errors)
cargo make carbide-lints       # Custom lints (requires nightly setup)
cargo make check-format-nightly # Check rustfmt formatting
cargo make check-event-names    # Validate production Event identity uniqueness
cargo make check-metric-docs    # Check production Event metric catalogue coverage
cargo make check-workspace-deps # Validate dependency declarations in Cargo.toml
cargo make check-licenses      # Validate no restricted licenses introduced
cargo make check-bans          # Check for banned dependencies

# Optional maintenance check (not part of required CI or pre-commit):
cargo make check-isolated-package-builds # Check each package with default features

# Auto-fix formatting:
cargo fmt --all
cargo make format-nightly      # Also sort imports
```

> **Note:** The nightly toolchain is used only for `check-format-nightly` and
> `carbide-lints`. The stable toolchain pinned in `rust-toolchain.toml` is used
> for everything else.

### Top-level Makefile entrypoints

A top-level [`Makefile`](Makefile) at the repo root provides a thin
discoverable entrypoint for selected Core workflows and the `rest-api/` Go
services. It delegates to cargo-make or `rest-api/Makefile`.

```bash
make help                # default goal: list available targets
make core/check-isolated-package-builds # optional independent default-feature builds
make rest-build          # build rest-api Go binaries
make rest-test           # run rest-api unit tests (starts Postgres and mock gRPC servers)
make rest-lint           # lint rest-api
make rest-fmt            # go fmt check on rest-api
make rest-helm-lint      # helm lint rest charts
make rest-docker-build-local
make rest-kind-reset     # spin up the local kind dev cluster (~10 min)
make rest-api/<target>   # pass any target through to rest-api/Makefile
```

Test the `rest-api/` Go services through these targets or the `rest-api/Makefile` ones they
delegate to, not by calling `go test` yourself. The targets start the PostgreSQL container and
the mock Core and Flow gRPC servers the tests connect to, and skipping that setup does not fail
fast: the `site-agent` tests retry on a `40s` backoff until the `10m` test timeout. See the
[Testing section in `rest-api/AGENTS.md`](rest-api/AGENTS.md#testing) for which target covers
which module.

Published container artifacts must pin external base images by immutable
digest. When architecture-specific targets share a base image, define one
overridable variable so their versions cannot drift independently.

## Coding Conventions

Follow the shared [Engineering Guidelines](CONTRIBUTING.md#engineering-guidelines)
for scope control, reuse-before-new-code, evidence-backed assumptions, and
verification expectations.

See [`STYLE_GUIDE.md`](STYLE_GUIDE.md) for detailed Rust coding conventions.
Make sure to review it to ensure changes meet the expected style of the codebase.

Use the narrowest Rust visibility required by actual callers. Do not use `pub`
to suppress dead-code warnings or widen production visibility solely for unit
tests. Follow the [visibility guidance](STYLE_GUIDE.md#visibility).

### Database migrations

Core migrations live in `crates/api-db/migrations/` and use the fully populated
`YYYYMMDDhhmmss_description.sql` format described in
[`STYLE_GUIDE.md`](STYLE_GUIDE.md#database-migrations). The `migration-police`
CI job checks only newly added migrations, so existing filenames remain
accepted.

Most of what makes a migration wrong here is not something CI can catch, because
an over-built migration still applies cleanly. Write the smallest exact forward
change from the schema on `main`: the median migration in this repository is two
statements and eight lines, and most do exactly one thing.

- **Never edit a migration that has merged.** Its checksum is recorded by every
  deployment that has applied it. Assume any migration on `main` is already
  running somewhere, since this is a public repository and we cannot know where.
  Correct it with a new forward migration instead.

- **Do not hand-roll safety the runner already provides.** `sqlx` runs each file
  in a transaction and records it once, so a migration cannot half-apply or run
  twice. Leave out `IF EXISTS` and `IF NOT EXISTS`, `ON CONFLICT DO NOTHING`
  added for rerunnability, `DO $$ ... RAISE EXCEPTION` preflight checks, system
  catalog probes, `BEGIN` and `COMMIT`, and `NOT VALID` paired with
  `VALIDATE CONSTRAINT` in the same file. These pass CI and read as caution, but
  they convert a diagnosable failure into a silent skip.

- **The migration has to apply while a `nico-api` instance is still running.**
  Nothing stops the running instance first, so assume a live writer throughout.
  A migration's contract is with the incoming binary, so leave the schema that
  version expects rather than preserving the outgoing one, but it still has to
  succeed against that writer.

- **Keep domain logic in Rust.** Do not reimplement a Rust helper in PL/pgSQL,
  and do not add a view, function, or trigger without a live consumer or an
  invariant the database has to own.

- **Use `NULL` for genuine absence.** Add a column as nullable, or `NOT NULL`
  with a default that is true for every row it reaches. Do not invent an empty
  string, an epoch timestamp, a placeholder version, or `UNKNOWN` to satisfy
  `NOT NULL`. A `jsonb` default has to deserialize into the current Rust type.

- **Remove things in two steps.** Ship the code that stops reading and writing
  it, then drop it in a later migration.

- **Comment intent, briefly.** A sentence or two on why the change exists. A
  safety argument or design rationale belongs in the pull request, not in the
  migration.

- **Test anything with data semantics** against a database at the predecessor
  schema populated with realistic rows, including a backfill, a default, a new
  requiredness, a constraint, or a type conversion. Reference the file with
  `include_str!` so the test cannot drift from what ships, as
  [`test_backfill.rs`](crates/api-db/src/credential_rotation/test_backfill.rs)
  does. A purely additive nullable column is already covered by the suite.

### Documentation

Give every fenced code block a language identifier. Use `bash` or `sh` for
shell commands and `text` for command output or other unformatted examples.

### Operator documentation

When documenting Helm-backed settings, identify chart values as defaults when
the templates allow overrides. State actual precedence when Helm-provided flags
override binary environment-variable fallbacks.

### Instrumentation: logs and metrics

The decision rule:

- **Just logging words?** Use plain `tracing::` macros with structured fields
  (`warn!(%machine_id, error = %e, "...")`). Most log sites are and stay this.
- **Does the event deserve a count, rate, or duration** (a failure you'd alert
  on, an outcome you'd trend, a hot-path rate)? Declare it once as a
  `carbide_instrument::Event` and `emit()` it — that produces the metric and
  (optionally) the log line together, correlated by `span_id`:

  ```rust
  #[derive(carbide_instrument::Event)]
  #[event(event_name = "power_control_failed",
          metric_name = "carbide_power_control_total", component = "component_manager",
          log = warn, metric = counter, message = "power control failed",
          describe = "Number of power control operations that failed")]
  struct PowerControlFailed {
      #[label]   backend: Backend,  // bounded via LabelValue — enums, usually
      #[context] error: String,     // high-cardinality — log line only
  }

  carbide_instrument::emit(PowerControlFailed {
      backend: Backend::Rms,
      error: "deadline exceeded".to_string(),
  });
  ```

  `log = off, metric = counter` counts a hot-path event with no log line at
  all; `metric = none` is a typed log. Never put `machine_id`/IPs/error text
  in a `#[label]` — that's what `#[context]` is for, and `String` doesn't
  implement `LabelValue` precisely to stop it. A bounded-but-not-enum value
  (a vendor, a SKU) can get a manual `LabelValue` impl on a newtype — the
  reviewed escape hatch — but only when the value is bounded *at the call
  site*; anything caller-supplied stays in `#[context]`.
- **Point-in-time state** ("how many machines are in state X") stays on the
  existing observable-gauge / `SharedMetricsHolder` pattern — the framework models
  occurrences, not state.

Every Event declares a unique, flat `lower_snake_case` `event_name`. It identifies
the reusable event category, not one occurrence, and appears on Event-generated
logs. A metric-backed Event also declares `metric_name`; when that Event logs,
both names are present so operators can pivot directly between the metric and its
diagnostic records. Plain `tracing::` calls do not invent an `event_name`.

New metric names are checked at compile time (`carbide_` prefix, `_total`
counters, unit-suffixed histograms), and a checked `metric_name` is exposed
verbatim. Existing metric contracts never change. The full standard lives in
[`docs/observability/instrumentation.md`](docs/observability/instrumentation.md).

## Documentation review

These are documentation checks, not style guidance. Apply every relevant
check before requesting review.

- **Interface contracts:** Document the complete contract, not just the name or happy path.

  - For every documented command, flag, environment variable, config key, API field, mode, or state, verify the following from code, schema, or exercised output:
    - exact spelling and case
    - required or optional condition
    - default
    - accepted values, units, formats, and bounds
    - mutual exclusions and interactions
    - global versus subcommand position and required order
    - omission or fallback behavior
    - observable output, side effects, errors, and unsupported paths
    - for repeated or list fields, membership, ordering, and whether omitted and
      empty values have the same meaning
  - Exercise each changed CLI example at the PR revision on an authorized local
    or test target and compare it with real `--help` output. Verify changed API,
    configuration, environment-variable, and state contracts through schemas,
    handlers, or exercised output appropriate to that interface.
  - If any answer is unknown, stop and ask the owner; another documentation page
    is not evidence.

- **Generated interfaces:** Change the source, regenerate every output, and prove they stay in sync.

  - Never edit a generated reference.
  - For `nico-admin-cli`, change the Clap declarations under
    `crates/admin-cli/src/`, verify the affected command with
    `cargo run -q -p nico-admin-cli -- <command-path> --help`, then run
    `cargo make gen-cli-docs` and `cargo make check-cli-docs`. See
    `crates/admin-cli/AGENTS.md` for the generated and hand-authored boundaries.
  - For REST, use `rest-api/openapi/spec.yaml` for the contract and inspect the
    handler or model for conditional behavior the schema cannot express. When
    the spec changes, run `make rest-api/lint-openapi`,
    `make rest-api/generate-sdk`, `make rest-api/publish-openapi`, and
    `make openapi-breaking`; do not edit `rest-api/sdk/standard/` or
    `rest-api/docs/index.html`.

- **Workflow parity:** Make the documentation match the workflow that actually runs.

  - For setup documentation, `helm-prereqs/setup.sh` is the source of truth for
    phases, skip flags, environment requirements, and component order. Run
    `bash -n helm-prereqs/setup.sh` and cross-check `helm-prereqs/README.md` and
    `docs/getting-started/quick-start.md`.
  - For state-machine documentation, trace every success, skip, retry, poll,
    restart, deletion, maintenance, and error transition to its enum and
    handler. Narrative, Mermaid, and transition tables must contain the same
    states and edges, including persisted resume state.

- **Metric catalogue:** A metric is not documented until its HELP text, emitted series, and generated catalogue agree.

  - Treat metric HELP text and `docs/observability/core_metrics.md` as generated
    API documentation.
  - Verify what causes the observation, its counter/gauge/histogram type, the
    exact entity and condition measured, direction or protocol, and every label
    dimension.
  - New metrics need non-empty `describe` text and must be exercised by
    `test_integration` so catalogue generation includes their exposed name,
    type, and description. Do not patch the generated table.

- **Cross-surface drift:** Change a fact everywhere it appears or make one page canonical and link to the canonical page from the others.

  - When a documentation objective names implementation or issue links, verify
    that the final document retains those links at the exact review head.
  - Search every changed literal or behavior with
    `rg -n --fixed-strings '<literal>' README.md crates/ rest-api/ docs/ book/ helm/ helm-prereqs/ deploy/`;
    reconcile every conflicting hit or establish one canonical explanation and
    link the others to it.
  - New or moved public pages must be present in `docs/index.yml`, and moved or
    removed public paths need redirects in `fern/docs.yml`.
  - Using the CLI version pinned in `fern/fern.config.json`, run
    `fern docs md check` and `fern check` from the repository root. Neither
    command needs a Fern token, but without one, `fern check` skips the
    published-state `missing-redirects` check.
  - `fern/changelog/` owns current unified releases;
    do not modify `rest-api/CHANGELOG.md`, as it is legacy history whose
    published entry order must be preserved.

- **Temporary claims and versions:** Tie temporary claims and pinned versions to a real support boundary.

  - Search changed prose for `currently`, `today`, `for now`, `temporarily`, and
    `draft`. Each match must name a release, deliberate support boundary, or
    full tracking URL; a bare issue number or review-history note is
    insufficient.
  - Hard-code a tool or dependency version only when it is a tested minimum,
    maximum, or pinned compatibility boundary; otherwise link the authoritative
    release page.

- **Rendered output:** Review each artifact in the renderer its readers actually use.

  - Run `rumdl check --config docs/.rumdl.toml AGENTS.md` for this file, and pass
    every other changed Markdown path as an additional argument. Fail on every
    finding.
  - For Fern-published pages, inspect the CI-created PR preview. A local
    `fern docs dev` preview works without a Fern token, but does not apply the
    global `nvidia` theme without one.
  - If a hosted preview must be created manually, run
    `preview_id='nico-docs-review'; fern generate --docs --preview --id "$preview_id"`,
    then delete it after review with
    `fern docs preview delete --id "$preview_id"`.
  - Inspect generated `nico-admin-cli` pages in GitHub's renderer and OpenAPI
    output with `make rest-api/preview-openapi`. Check navigation, anchors, wide
    tables, Mermaid layout, and component rendering in the actual target; lint
    success is not rendered verification.

## Further Reading

- [`README.md`](README.md) — Project overview and getting started
- [`STYLE_GUIDE.md`](STYLE_GUIDE.md) — Detailed Rust coding conventions
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — Contribution workflow and DCO process
- [`docs/architecture/overview.md`](docs/architecture/overview.md) — Architecture overview
