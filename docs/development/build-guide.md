# Build Guide

## Native Linux ARM64 Builds

The repository's [Cargo configuration](../../.cargo/config.toml) requests mold
when building the `aarch64-unknown-linux-gnu` target. On native Linux ARM64
hosts, [`.envrc`](../../.envrc) selects Clang as the compiler driver. Before
running Cargo directly, ensure that the `clang` and `mold` executables are on
`PATH`. On Ubuntu 24.04 or Debian 12 (Bookworm) and newer, install both packages
with:

```bash
sudo apt-get install -y clang mold
```

Without either executable, Cargo fails during linking. If you do not use
`direnv`, select Clang in the shell that runs Cargo:

```bash
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=clang
```

Debian 11 (Bullseye) does not package mold. Use the containerized ARM64 build
path for Bullseye-compatible artifacts; its
[build image](../../dev/docker/Dockerfile.build-artifacts-container-aarch64)
installs a checksum-verified upstream ARM64 release. Other containerized ARM64
build paths also install both tools in their build images and do not require
them on the host.

## Updating Pinned Dependencies

### Git submodules

Two git submodules are pinned to known-good versions:

| Submodule | Path | Pinned version |
|-----------|------|----------------|
| mkosi | `pxe/mkosi` | `v25` |
| iPXE (upstream) | `pxe/ipxe/upstream` | `v2.0.0-130-gbbd7821bd8` |

To update a submodule to a newer version:

```bash
cd pxe/mkosi          # or pxe/ipxe/upstream
git fetch
git checkout <new-tag-or-commit>
cd ../..
git add pxe/mkosi     # or pxe/ipxe/upstream
git commit -s -m "chore: bump mkosi to <new-version>"
```

After bumping, validate with a full PXE artifact build:

```bash
cargo make build-pxe-build-container   # rebuild if Dockerfile changed
cargo make pxe-docker-x86
```

### Rust toolchain

The Rust compiler version is pinned in `rust-toolchain.toml`. To update, change the version there and update the `RUST_VERSION` ARG in `dev/docker/Dockerfile.pxe-build-container` to match.

## Testing the NICo Image

After building the `nico` release image, run a quick sanity check to confirm all binaries are
present and start without crashing:

```bash
for bin in nico nico-admin-cli nico-api nico-dns nico-dsx-exchange-consumer \
           nico-dhcp-server nico-dpu-agent nico-hw-health nico-log-parser ssh-console; do
  echo "$bin: $(docker run --rm nico /opt/nico/$bin --help 2>&1 | head -1)"
done
```

Each line should print a usage string or a startup log line. Services that don't implement
`--help` (e.g. `nico-dsx-exchange-consumer`, `nico-hw-health`) will log their startup config
and then block waiting for connections — that is expected and counts as a pass. Any
`exec format error` or `No such file` indicates a broken build.

## Build Optimizations and Trade-offs

The Docker release image build (`Dockerfile.release-container-sa-x86_64`) includes several
non-obvious optimizations. This section documents the intent and trade-offs so future maintainers
understand why the build is structured the way it is.

### `debug = "line-tables-only"` in the release profile

**What it does:** The release profile uses `debug = "line-tables-only"` instead of the Rust default
(`debug = 0`) or full debug info (`debug = true`). This embeds line-number tables in binaries but
omits DWARF variable info (local variable names, types, values).

**Why:** With `debug = true`, the `nico-api` binary alone was ~1.46 GB, producing a 5.4 GB
release image. `"line-tables-only"` reduced the binary to ~544 MB and the image to ~2.5 GB —
a 58% reduction — while keeping stack traces useful (line numbers are preserved).

**Trade-off:** Debuggers (gdb/lldb) and core dump analysis will show the call stack with line
numbers but will not be able to inspect local variable values. For production debugging this is
usually acceptable because we rely on structured logging and tracing rather than debugger sessions.
If you need full variable inspection (e.g., for post-mortem core analysis of a reproduction), build
locally with `debug = true` in a `[profile.dev]` override or a local `Cargo.toml` override.

Release container builds override the default back to full debug info via the
`CARGO_PROFILE_RELEASE_DEBUG=true` environment variable in the Dockerfiles.

### Workspace clippy and targeted release builds

**What it does:** `clippy-release` is invoked with `cargo make --no-workspace` in the SA
Dockerfile, so cargo-make runs clippy once at the workspace root instead of once per workspace
member. `build-release` is itself a non-workspace task and explicitly selects the packages that
produce artifacts copied into the release container.

**Why:** The per-member iteration caused shared dependencies (`tonic`, `sqlx`, `nico-rpc`, etc.)
to be recompiled repeatedly across members. Switching to `--no-workspace` reduced the build from
~98 minutes to ~21 minutes on a 72-core server. Selecting the production packages also prevents
test-only workspace members from enabling test-support features in production dependencies.

Keep the package selection in `build-release` synchronized with the explicit artifact list in the
`Dockerfile.release-container-*` files. A clean container build fails at its `COPY --from=builder`
step if a required artifact is missing.

### `clippy-release` shares artifacts with `build-release`

**What it does:** The `clippy-release` Makefile task runs clippy with `--release`, and
`build-release` also compiles the production packages in release mode. Cargo reuses compatible
`.rlib` and `.rmeta` artifacts from the clippy step. A dependency whose production feature set
differs from clippy's `--all-features` graph is rebuilt with the narrower feature set.

**Trade-off:** `clippy-release` passes `--all-targets`, which includes test and benchmark targets
that `build-release` does not compile. Clippy therefore lints slightly more code than is shipped
in the final binary. In practice this is a net benefit (broader coverage), but if a test-only
dependency activates features that interact unexpectedly with production code, the lint results
may differ from a targeted per-crate clippy run.
