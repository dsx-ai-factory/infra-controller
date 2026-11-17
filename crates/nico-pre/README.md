# nico-pre

Pre-flight checks an Infrastructure Provider runs at each stage of a NICo deployment. One
binary with one subcommand per stage (`nico-pre config`, or `nico-pre-config` through a
hard link or copy of that name).

Terms:

- **site-def.yaml**: the one file the provider writes for the tool. It states the decisions,
  where the NICo configuration files live, and the plan values the checks compare them with.
- **register**: the decision register compiled into the binary (`register/`): every decision a
  site has to make, its options and its default. `decisions:` in site-def.yaml answers it.
- **checklist**: the checks of one stage; one subcommand each: `config`, `build`, `stage`,
  `host`, `fabric`, `platform`, `install`, `ingest`. A checklist is a table in
  `src/checklists/<name>.rs`.
- **check**: one entry of a checklist and one row of its output: expected value,
  observed value, ✓ ⚠ ✗, a fix. Developers add checks to a checklist, or a new checklist.
- **gate**: the site-def.yaml checks every subcommand runs before its checklist (`validate`
  runs only these).

Provider-facing design: the `nico-pre` guide in the documentation tree (issue #4053). This
crate is the foundation: the site-definition schema, the decision register compiled in
(`register/`), the gate, the row and result model, output, and the checklist skeleton.

```bash
cargo run -q -p nico-pre -- decisions   # print the register: every decision, its options and default
cargo run -q -p nico-pre -- checks                          # every check by id, implemented or not; --format md writes CHECKS.md
cargo run -q -p nico-pre -- decisions --template   # print a decisions: section for site-def.yaml, to fill in
cargo run -q -p nico-pre -- validate --site-def crates/nico-pre/examples/site-def.yaml   # the gate alone: site-def.yaml checks, no checklist
cargo run -q -p nico-pre -- config --site-def crates/nico-pre/examples/site-def.yaml   # a pre-flight run: the gate, then the config checklist
```

The site-definition schema is documented key by key in [`SITE-DEF.md`](SITE-DEF.md).
`examples/site-def.yaml` is the illustrative sample; `examples/site-def-dev1.yaml` describes the
nico-dev `dev1` developer site. The register compiled in under `register/` is the source of
truth for the decisions, their options and the rules between them; edit it in the same PR as
the checks that read it.

## Building and installing the binary

`cargo run` above builds and runs `target/debug/nico-pre` at the workspace root; with
`--release` the binary is `target/release/nico-pre`. It is an ordinary executable: nothing of
Rust is needed where it runs, and every dependency is pure Rust, so it links only the C
library of its platform. Two conditions for copying it to a staging host:

- **Same operating system and architecture.** A macOS build does not run on Linux; build
  Linux x86_64 or aarch64 for a provider host.
- **glibc on the target at least as new as on the builder.** On a Linux x86_64 machine with
  the Rust toolchain, `cargo build -p nico-pre --release` is enough when the target's glibc is
  not older than the builder's. Otherwise, and always from macOS, build inside the repository's
  Linux build container. This needs Docker and `cargo-make` installed; the first command builds
  the `carbide-build-x86_64` image from `dev/docker/Dockerfile.build-container-x86_64` (slow on
  Apple Silicon, once), the second runs Cargo inside it with the repository and your Cargo home
  mounted, which is all `cargo-docker` means. On Apple Silicon export the platform first: the
  base image is multi-architecture and Docker would otherwise pick arm64 and produce an aarch64
  binary.

  ```bash
  export DOCKER_DEFAULT_PLATFORM=linux/amd64
  ```

  ```bash
  cargo make build-cargo-docker-image
  ```

  ```bash
  cargo make cargo-docker -- build -p nico-pre --release
  ```

  The result is `target/release/nico-pre`, a Linux x86_64 binary; copy it with `scp` and run
  it from any directory.

A statically linked binary per architecture, released at the NICo tag, is the goal of the
epic (issue #4053) and removes the glibc condition; that is release-pipeline work tracked in
its CI sub-issue (#9072). Until then the container build is the supported way to produce a
binary for a provider host.

## Writing a check

A checklist is a table in `src/checklists/<name>.rs`; each entry is one check, one row of the
output and one task for a coder. The harness (`src/harness.rs`) runs the table, times each check, turns
an error or a panic into a `!` row for that check alone, honours `--only`/`--skip`, prints, and
writes the result file. To land a check:

1. Find its entry in the table (registered as `run: todo`, shown as ⚠ "not implemented yet")
   or add one: a stable id `<checklist>.<area>.<name>`, the expected value as the row prints
   it, and `detail`, the spec: what it reads, when it passes, warns, fails or skips, and how
   it is tested. Every registered entry already carries one; implement against it.
2. Write `fn <name>(ctx: &Ctx) -> eyre::Result<Outcome>` in the same file. `ctx.sd` is the site
   definition, `ctx.resolve(path)` a collateral path, `ctx.decided(id, option)` a decision.
   Return `Pass(observed)`, `Warn { observed, fix }`, `Fail { observed, fix }` or `Skip(why)`
   when a decision rules the check out. Use `?` for anything that stops the check from running;
   never panic on site data.
3. Point the entry at it and add a test next to it against the fixture the spec's `Test:`
   line names (a bmc-mock profile, the dev1 collaterals, a kind cluster), with at least the
   cases it lists.
4. Regenerate the catalogue: `cargo run -q -p nico-pre -- checks --format md > crates/nico-pre/CHECKS.md`.
   A test fails while it is stale.

`config.segments.admin` in `src/checklists/config.rs` is the worked example;
[`how-to-add-checks.md`](how-to-add-checks.md) walks through it: where the code goes, what the
context injects, what to return, how to test.

Exit codes: 0 every check passed; 1 warnings only; 2 at least one check blocks; 3 the tool could
not run a check, or the site definition failed the gate.
