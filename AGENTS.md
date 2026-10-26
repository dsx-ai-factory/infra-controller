# AGENTS.md

NVIDIA Infra Controller (NICo) provides site-local, zero-trust bare-metal
lifecycle management through Rust and Go APIs, with DPU-enforced isolation.
It handles inventory, Redfish management, hardware testing and firmware, IP/DNS,
power control, provisioning, wiping, release, and trust during Tenant switching.
APIs, configurations, and features may change between releases. See the
[architecture](docs/architecture/overview.md) for component responsibilities. Use
[dsx-ai-factory/infra-controller](https://github.com/dsx-ai-factory/infra-controller)
for repository, PR, issue, and source links.

## Scope and precedence

Follow the user's requested outcome and applicable instructions. Read the nearest
`AGENTS.md` for each affected component and the relevant sections of
[STYLE_GUIDE.md](STYLE_GUIDE.md) before changing Rust, Go, or protobuf code.
Explicit exceptions here, including test selection below, narrow the style guide.
For repository conventions, follow `STYLE_GUIDE.md` over conflicting user-global
`AGENTS.md` guidance. Explicit task instructions still take precedence.

Follow [Engineering Guidelines](CONTRIBUTING.md#engineering-guidelines) for scope,
reuse, evidence, and verification. Preserve others' work. Do not edit any
`AGENTS.md` file or `STYLE_GUIDE.md` unless the user requests those changes.
Never commit secrets or local environment files; keep local secrets in
gitignored `.local_envrc` and follow [secret scanning](CONTRIBUTING.md#secret-scanning).

When asked to update agent guidance, add only durable repository constraints or
required workflows that apply beyond a single PR. A review incident may reveal
such a constraint; record the general rule, not the incident. State its scope,
action, and consequence, backed by code, CI, a contract, or a documented
maintainer decision. Keep PR narratives, review disputes, and PR-specific
workarounds or exceptions in the relevant PR or issue, not in `AGENTS.md`.
Keep component-only rules with the component and shared rules here; link
canonical procedures instead of repeating them.

## Find the authoritative source

- Rust implementations: `crates/`; package names, descriptions, and dependencies:
  the workspace and individual `Cargo.toml` files. Directory and package names
  can differ (`crates/api/` is `nico-api`); check before using `cargo -p`.
- Rust toolchain: `rust-toolchain.toml`; formatting/custom-lint nightly:
  `Makefile.toml`. Go version: the applicable `go.mod`.
- Core uses Tokio, Tonic with Rustls/aws_lc_rs, Axum, SQLx, OpenTelemetry,
  structured logfmt Tracing, and protobuf. See `Cargo.toml` for versions and
  compatibility constraints, including the Axum pin.
- Build, packaging, and shared tasks: `Makefile*.toml` and `include/`; CI:
  `.github/workflows/ci.yaml`; custom Clippy lints: `lints/`.
- Helm charts: `helm/`; local tools, test configs, and certificates: `dev/`; DPU
  components: `bluefield/`; boot artifacts: `pxe/`; mdBook content: `book/`.
- REST commands and conventions: [rest-api/AGENTS.md](rest-api/AGENTS.md).
- CLI help/reference generation: [crates/admin-cli/AGENTS.md](crates/admin-cli/AGENTS.md).
- Operator documentation: `docs/`, with navigation in `docs/index.yml` and Fern
  configuration in `fern/`. See [development setup](docs/development.md) for prerequisites.

## Build, test, and lint

Install `cargo-make` with `cargo install cargo-make`. Use the task definitions and
[verification guidance](CONTRIBUTING.md#verification) for prerequisites, scope,
and reporting results. Core workflows primarily use cargo-make; use direct Cargo
commands where listed below. REST workflows use the top-level or `rest-api/`
Makefiles.

| Purpose | Command |
| --- | --- |
| Debug or release Rust build | `cargo build` / `cargo build --release` |
| Admin CLI build | `cargo make build-cli` |
| All Rust tests | `cargo test` |
| Rust tests, with debug prerequisites | `cargo make correctly-execute-tests` |
| Focused Rust test after prerequisite setup | `cargo test -p <package> <test-filter>` |
| Full local Core verification | `cargo make pre-commit-verify` |
| Workspace lint, format, and dependency checks | `cargo make pre-commit-verify-workspace` |
| Core release artifacts and tests | `cargo make build-and-test-release-container-services` |
| Format using the CI rustfmt options | `cargo make format-nightly` |
| Format with stable rustfmt | `cargo fmt --all` |
| Check formatting | `cargo make check-format-nightly` |
| Clippy / custom lints | `cargo make clippy` / `cargo make carbide-lints` |
| Event identity / metric catalogue | `cargo make check-event-names` / `cargo make check-metric-docs` |
| Dependencies / licenses / bans | `cargo make check-workspace-deps` / `cargo make check-licenses` / `cargo make check-bans` |
| Optional independent default-feature package builds | `cargo make check-isolated-package-builds` |

Clippy treats warnings as errors. `make help` lists top-level entrypoints,
including `make core/check-isolated-package-builds`. REST uses `make rest-build`,
`make rest-test`, `make rest-lint`, `make rest-fmt`, `make rest-helm-lint`,
`make rest-docker-build-local`, `make rest-kind-reset`, or `make rest-api/<target>`.
Run Go service tests through these Make targets, which start required PostgreSQL
and mock Core/Flow gRPC services. Do not run `go test` directly: Site Agent tests
can retry every `40s` until the `10m` timeout instead of failing promptly. See
[REST testing](rest-api/AGENTS.md#testing) for module-specific targets.

Use the stable toolchain for ordinary builds/tests. `format-nightly`,
`check-format-nightly`, and `carbide-lints` use the pinned nightly task setup.
Independent package builds are optional maintenance, not a required CI gate.

## Test selection

Inventory relevant existing tests before adding coverage. Each new test or row
must protect an observable contract or distinct failure boundary that retained
coverage does not protect. Cover reachable combinations when they change behavior
or precedence; do not enumerate a Cartesian product just because inputs are enums
or booleans. Remove or merge redundant cases.

Put each proof at the narrowest layer that can exercise it. Higher-level tests
should add wiring, persistence, transactions, concurrency, or external-effect
coverage rather than repeat a lower-level matrix. These selection rules narrow
STYLE_GUIDE.md's advice to enumerate branches/input variants; its table helpers
and layout remain authoritative.

Prefer table-driven tests and `carbide-test-support` helpers; see
[Testing](STYLE_GUIDE.md#testing) for helper APIs and table layout. Use grouped
`scenarios!` / `value_scenarios!` or explicit `check_cases` / `check_values` when
cases share one operation and assertion form. When cases share setup but need
different assertions, use a local case table with each check beside its inputs.

- State-machine branch tests: reload persisted state after the controller
  iteration and assert branch-owned fields/counters. Unchanged visible state or
  no external call alone does not prove the branch ran.
- Retry tests: inject the claimed transient failure, then prove a later iteration
  retries while preserving the expected externally visible state. A simulator's
  unsupported response does not prove transient recovery.
- CLI table changes: test the public command's rendered headers and populated and
  empty cell values, not only a formatting helper.
- Rack fixtures: align capability counts with instantiated inventory; use zero
  for unsupported component types.

## Contracts and implementation

Use the narrowest Rust visibility required by actual callers. Do not widen
production APIs just for tests or to silence dead-code warnings; see
[visibility](STYLE_GUIDE.md#visibility).

REST response fields preserve empty values. Use `omitempty` only for date-gated
attribute removal or opt-in `includeRelation` expansions; see the scoped REST
guide for those contracts. Paginated operations must order results before slicing
and document supported `orderBy` values and defaults in OpenAPI. Do not depend
on implicit upstream ordering.

Published container artifacts must pin external base images by digest. Share one
overridable base-image variable across architecture-specific targets that use
the same image.

### Database migrations

Core migrations live in `crates/api-db/migrations/`. Follow
[migration naming](STYLE_GUIDE.md#database-migrations), using a fully populated
`YYYYMMDDhhmmss_description.sql` timestamp. Existing names remain accepted;
merged migration contents are immutable. Assume every migration on `main` is
already deployed; use a new forward migration for fixes.
CI checks new filenames and modifications/deletions of existing migrations.

Write the smallest exact forward change from the schema on `main`. Assume an
existing `nico-api` instance can still write while migrations run. Leave the schema
the incoming binary requires, and verify the transition succeeds with that writer.

- Ordinary SQLx migrations are transactional and recorded once. Leave out
  `BEGIN`/`COMMIT`, catalog probes, preflight exception blocks, `IF [NOT] EXISTS`,
  and `ON CONFLICT DO NOTHING` added for rerunnability. Do not pair `NOT VALID`
  with `VALIDATE CONSTRAINT` in the same file. Let schema drift fail visibly
  instead of hiding it with redundant guards.
- Operations such as concurrent index creation/removal require the existing
  `-- no-transaction` mechanism. Use it only for a statement requiring it; reason
  about interrupted application and recovery separately from the transactional
  default. Do not assume such a file is atomic.
- Keep domain logic in Rust; do not reimplement Rust helpers in PL/pgSQL. Add a
  database view, function, or trigger only for a live consumer or an invariant
  the database must enforce.
- Use `NULL` for absence. A `NOT NULL` default must be true for every row it
  reaches; do not invent sentinel strings, timestamps, or versions. JSON defaults
  must deserialize into the current Rust type.
- Remove in two steps: ship code that stops reading/writing an object, then drop
  it in a later migration.
- Comment intent briefly; put extended design/safety arguments in the PR.
- Test data-semantic changes against a populated predecessor schema: backfills,
  defaults, requiredness, constraints, and type conversions. Include the shipped
  SQL with `include_str!`, following
  [test_backfill.rs](crates/api-db/src/credential_rotation/test_backfill.rs).
  A purely additive nullable column needs no extra migration-specific test.

### Logs and metrics

Follow [instrumentation](docs/observability/instrumentation.md) and
[logging](STYLE_GUIDE.md#logging). Plain diagnostics use `tracing::` with structured
fields. Occurrences that need a count/rate/duration use one declared
`carbide_instrument::Event` and `emit()`, producing a metric and optional log.
Use `log = off` for metric-only events and `metric = none` for typed logs.
Point-in-time state keeps the observable-gauge/`SharedMetricsHolder` pattern.

Keep metric labels bounded at the call site. Machine IDs, IPs, error text, and
caller-supplied values belong in `#[context]`, not `#[label]`. A reviewed newtype
may implement `LabelValue` for non-enum values only when bounded at the call site.
Preserve existing metric contracts. Every Event declares a unique, flat
`lower_snake_case` `event_name` for its category. Metric-backed Events also declare
`metric_name`; both appear on their logs, correlated with the request by `span_id`.
New metric names use `carbide_`, `_total` for counters, and unit suffixes for
histograms. Checked names are exposed verbatim.
New metrics need meaningful, non-empty descriptions, even where the derive permits
omission. Plain tracing calls do not invent `event_name` or `metric_name` fields.

For checked-name framework counters/histograms, run `cargo make check-metric-docs`;
use `cargo xtask check-metric-docs --fix` for missing source-derived catalogue rows.
Exercise new API metrics in `test_integration` so its scrape generates their
catalogue entries. Exercise metrics owned by other binaries at their owning
integration layer. Treat HELP text and `docs/observability/core_metrics.md` as
generated API documentation. Verify the observed event, exact entity and condition,
direction/protocol, series type, units, and every label; do not hand-edit generated
catalogue rows. HELP text, emitted series, and catalogue must agree. Legacy
unchecked-name exemptions remain as documented.

## Documentation review

These are documentation checks. Apply every relevant check before requesting
review. Document the complete contract, including conditions and failure paths.

- Verify interface spelling/case, requiredness, defaults, formats/units/bounds,
  accepted values, mutual exclusions/interactions, global/subcommand position,
  required order, omission/fallback behavior, output, side effects, errors, and
  unsupported paths against code, schema, or exercised output. For repeated/list
  fields, verify membership, ordering, and omitted versus empty semantics.
  Documentation alone is not proof of implementation behavior.
- Exercise changed CLI examples at the reviewed revision on an authorized local
  or test target and compare actual `--help`. Verify API, configuration,
  environment-variable, and state contracts through schemas, handlers, or
  exercised output. If any answer is unknown, stop and ask the owner.
- Never edit a generated reference. Change interface sources, regenerate every
  affected output, and prove they stay in sync. For admin CLI help, follow
  [CLI generation](crates/admin-cli/AGENTS.md#documentation).
  Inherited global options can affect multiple command pages; inspect the output.
- For REST contracts, inspect `rest-api/openapi/spec.yaml` plus handlers/models.
  Run `make rest-api/lint-openapi`, `make rest-api/generate-sdk`, and
  `make openapi-breaking` when the spec changes. Do not hand-edit generated SDK
  files; hand-written helpers in `rest-api/sdk/standard/` are separate.
- Setup docs must match `helm-prereqs/setup.sh` phases, skip flags, environment
  requirements, and component order. Run `bash -n helm-prereqs/setup.sh` and
  cross-check `helm-prereqs/README.md` and `docs/getting-started/quick-start.md`.
- State-machine narratives, diagrams, and transition tables must agree with the
  relevant enums/handlers, including success, skip, retry, poll, restart, deletion,
  maintenance, errors, and persisted resume state.
- For Helm-backed settings, distinguish defaults from overrides and state actual
  flag/environment/config precedence.
- Search every changed literal or behavior with
  `rg -n --fixed-strings '<literal>' README.md crates/ rest-api/ docs/ book/ helm/ helm-prereqs/`.
  Reconcile every conflicting hit or link to one canonical explanation. Preserve
  required implementation/issue links in the final document at the exact review head.
- Register new/moved public pages in `docs/index.yml`; add redirects for moved or
  removed public URLs in `fern/docs.yml`. Current releases belong in
  `fern/changelog/`; do not modify legacy `rest-api/CHANGELOG.md` or its entry order.
- Search changed prose for `currently`, `today`, `for now`, `temporarily`, and
  `draft`. Tie each claim to a release, deliberate support boundary, or full
  tracking URL; a bare issue number or review-history note is insufficient.
  Pin prose versions only for a tested minimum, maximum, or compatibility boundary;
  otherwise link to the authoritative release page.
- Give fenced blocks a language (`bash`/`sh` for commands, `text` for output).
  Run `rumdl check --config docs/.rumdl.toml AGENTS.md <other-changed-markdown-paths>`
  and fail on every finding. Use the Fern CLI pinned in `fern/fern.config.json` for
  `fern docs md check` and `fern check` from the repository root. Neither needs a
  token; without one, `fern check` skips the published-state missing-redirects check.
- Inspect each artifact in its readers' renderer, including navigation, anchors,
  wide tables, Mermaid, and components: the CI Fern preview, GitHub for CLI pages, or
  `make rest-api/preview-openapi` for REST. Local `fern docs dev` omits the NVIDIA
  theme without a token. Lint is not rendered verification.

If a hosted Fern preview must be created manually, create it and remove it after
review:

```bash
preview_id='nico-docs-review'
fern generate --docs --preview --id "$preview_id"
fern docs preview delete --id "$preview_id"
```

See [README.md](README.md) for orientation and
[CONTRIBUTING.md](CONTRIBUTING.md) for contribution/signing requirements.
