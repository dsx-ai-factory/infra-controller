# Guide: Cargo via Docker on macOS

This guide describes what is in place for running Cargo (build, test, check) via Docker on macOS and how to use it when native Cargo is problematic (e.g. Rust version mismatch, `tss-esapi` or other platform issues).

---

## What’s in place

| Item | Location | Purpose |
|------|----------|--------|
| **Makefile task: `cargo-docker-minimal`** | `Makefile.toml` | Run Cargo in the minimal image (`nico-build-minimal`: Rust 1.97 + protoc). **Recommended on Mac.** Requires `build-cargo-docker-image-minimal` once. |
| **Makefile task: `build-cargo-docker-image-minimal`** | `Makefile.toml` | Build the minimal image (Rust + protoc only). Quick (~2–5 min). Required once for workspace builds (e.g. `nico-rpc` needs `protoc`). |
| **Makefile task: `cargo-docker`** | `Makefile.toml` | Run Cargo inside the repo’s full build container (`nico-build-x86_64`). Requires building that image first. |
| **Makefile task: `build-cargo-docker-image`** | `Makefile.toml` | Build the full Linux build image from `dev/docker/Dockerfile.build-container-x86_64`. Slow on Apple Silicon (45+ min). |
| **This guide** | `docs/manuals/cargo-via-docker-macos.md` | How to use the above and when to choose which option. |
| **Minimal Dockerfile** | `dev/docker/Dockerfile.cargo-docker-minimal` | Rust 1.97 + `protobuf-compiler` + `libprotobuf-dev` (well-known types). Used for `nico-build-minimal`. |
| **Full build Dockerfile** | `dev/docker/Dockerfile.build-container-x86_64` | Defines the full build image (Rust 1.97, PostgreSQL, protobuf, TSS, etc.). |

All commands below are run from the **repository root** unless noted.

---

## Prerequisites

1. **Docker**  
   Docker Desktop for Mac (or another Docker runtime) installed and running.

2. **cargo-make**  
   Used to run the Makefile tasks. Install if needed:
   ```bash
   cargo install cargo-make
   ```

3. **CARGO_HOME (optional but recommended)**  
   So the container can reuse your Cargo cache:
   ```bash
   export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
   ```

---

## Colima configuration (Apple Silicon M1/M2/M3)

If you use **Colima** on an M3 (or M1/M2) Mac, these settings can make `cargo-docker-minimal` and tests run faster.

**Recommended start command (one-liner)**

```bash
colima start --arch aarch64 --cpu 4 --memory 12 --disk 60 --vm-type=vz --mount-type=virtiofs
```

Use `colima stop` first if Colima is already running, then run the command above. This gives native arm64, 4 CPUs, 12 GiB RAM (avoids linker OOM when building `nico-api` tests), VZ virtualization, and virtiofs mounts. Adjust `--cpu` and `--memory` to match your machine.

**1. Use native arm64 (default)**  
Do **not** start Colima with `--arch x86_64`. The minimal image is built for your Mac’s architecture (arm64), so it runs natively. Forcing x86_64 would use emulation and be much slower.

**2. Give the VM more CPU and memory**  
Rust builds are CPU- and memory-heavy. Defaults (e.g. 2 CPU, 2 GiB RAM) are too low. Example for an M3:

```bash
colima stop
colima start --cpu 4 --memory 8 --disk 60
```

Adjust to your machine (e.g. 6–8 CPU, 8–12 GiB RAM if you have it).

**3. Use VZ runtime and virtiofs (macOS 13+)**  
Apple’s VZ virtualization and virtiofs mounts are faster than the default QEMU + 9p/sshfs setup:

```bash
colima stop
colima start --cpu 4 --memory 8 --disk 60 --vm-type=vz --mount-type=virtiofs
```

Or edit the config and start:

```bash
colima start --edit
```

In the editor, set (or add) something like:

```yaml
cpu: 4
memory: 8
disk: 60
vmType: vz
mountType: virtiofs
```

Then save, exit, and run `colima start`.

**4. Rebuild the minimal image after changing Colima**  
If you change CPU/memory or VM type, rebuild so the image uses the new VM:

```bash
cargo make build-cargo-docker-image-minimal
```

**Summary**

| Setting      | Recommended for M3   | Why                          |
|-------------|------------------------|------------------------------|
| Architecture | arm64 (default)       | Native; avoid x86_64 emulation |
| CPU         | 4–8                   | Parallel Rust compilation     |
| Memory      | 8–12 GiB              | Avoid OOM during large builds; if **linker is killed (signal 9)** when building `nico-api` tests, use 12 GiB |
| vmType      | `vz`                  | Faster than QEMU (macOS 13+)  |
| mountType   | `virtiofs`            | Faster volume I/O             |

The minimal image uses the **lld** linker (lower RAM use than default `ld`). If linking still fails with signal 9, give Colima more memory (e.g. `--memory 12`), then `colima stop` and `colima start` again, and rebuild the image: `cargo make build-cargo-docker-image-minimal`.

---

## Tests to run before build

Use these to validate changes (e.g. IB partition update) before building the Docker image or doing a full build.

**When to run what**

| Goal | What to run | Time |
|------|-------------|------|
| Daily sanity check (Docker + Postgres work) | `cargo make test-docker-postgres-smoke` (set `DATABASE_URL`) | ~10–30 s |
| IB partition / API behavior | `cargo make cargo-docker-minimal -- test -p nico-api test_update --no-default-features --no-fail-fast` (set `DATABASE_URL`) | First run: long (compile); **later runs: much faster** (sccache + incremental) |

Use the smoke test for quick feedback; run the IB partition tests when you need to validate that feature (e.g. before a PR). The minimal image uses **sccache** and a persistent cache dir (`~/.sccache` or `SCCACHE_DIR`), so the second and later runs of the same (or similar) commands are much faster.

### Option A: Quick check (no database)

Verifies that the API and deps compile. No Postgres needed.

```bash
# From repo root
cargo make cargo-docker-minimal -- check -p nico-api --no-default-features
```

If you haven’t built the minimal image yet, build it first: `cargo make build-cargo-docker-image-minimal`.

### Option A2: Postgres connectivity smoke test (small, fast)

Verifies that the minimal Docker setup can reach Postgres: `DATABASE_URL` is passed into the container and the DB is reachable (e.g. via `host.docker.internal`). Uses the small `postgres-smoke-test` crate so it compiles quickly.

**1. Start Postgres** (e.g. `docker-compose up -d postgresql` or use your existing Postgres). If you get an error about the `loki` logging plugin, start Postgres without it: `docker-compose -f docker-compose.yml -f docker-compose.no-loki.yml up -d postgresql`.

**2. Run the smoke test:**

```bash
export DATABASE_URL="postgres://nico_development:notforprod@host.docker.internal:5432/nico_development"
cargo make cargo-docker-minimal -- test -p postgres-smoke-test
```

Use the correct host/port (e.g. `host.docker.internal:30432` if Postgres is on port 30432 on the host). The `cargo-docker-minimal` task adds `--add-host=host.docker.internal:host-gateway` so the container can reach the host (required on Colima; harmless on Docker Desktop). If the test passes, Postgres connectivity from the container is working.

**Quick one-liner (recommended for a fast sanity check):**

```bash
export DATABASE_URL="postgres://nico_development:notforprod@host.docker.internal:5432/nico_development"
cargo make test-docker-postgres-smoke
```

Takes ~10–30 seconds. Use this instead of the long `nico-api` DB tests when you only need to confirm Docker + Postgres work.

### Option B: IB partition tests (need PostgreSQL)

These tests use a real database and **take a long time** (first run compiles most of the workspace). Prefer **Option A2** (smoke test) for a quick check; run these when you need to validate IB partition (or other API) behavior.

**1. Start Postgres (if not already running):**

```bash
docker-compose up -d postgresql
```

If you get an error about the `loki` logging plugin, use: `docker-compose -f docker-compose.yml -f docker-compose.no-loki.yml up -d postgresql`.

**2. Run IB partition tests:**

**Locally (Rust 1.97 + `DATABASE_URL`):**

```bash
export DATABASE_URL="postgres://nico_development:notforprod@localhost:5432/nico_development"
cargo test -p nico-api ib_partition --no-default-features --no-fail-fast
```

**Via minimal Docker image (after `build-cargo-docker-image-minimal`):**

```bash
export DATABASE_URL="postgres://nico_development:notforprod@host.docker.internal:5432/nico_development"
cargo make cargo-docker-minimal -- test -p nico-api ib_partition --no-default-features --no-fail-fast
```

Use `host.docker.internal` so the container can reach Postgres on the host (Docker Desktop for Mac supports this; the minimal image includes `libpq-dev` for the Postgres client).

**DATABASE_URL format:** Use the full URL including the database name (e.g. `.../nico_development`). The test harness connects to that database to create and drop per-test databases (e.g. `db0_nico__tests__...`). The database you name in the URL must already exist (create it or use an existing one such as `nico_development`).

**Other test filters:** The test name is a substring match. Examples:

| Filter        | What runs |
|---------------|-----------|
| `test_create` | Tests whose names contain `test_create` |
| `ib_partition` | All IB partition tests (lifecycle, find) |
| `update_ib`   | `test_update_ib_partition` only (partition update; use `update_ib`, not `ib_update`) |
| `update_instance_ib` | `test_update_instance_ib_config` only (instance IB config update) |
| `ib_`         | All IB-related tests (partition, instance, fabric, etc.) |

```bash
# Examples (set DATABASE_URL first)
cargo make cargo-docker-minimal -- test -p nico-api test_create --no-default-features --no-fail-fast
cargo make cargo-docker-minimal -- test -p nico-api update_ib --no-default-features --no-fail-fast
cargo make cargo-docker-minimal -- test -p nico-api update_instance_ib --no-default-features --no-fail-fast
```

### Suggested order before a build

1. Run **Option A** (check) to ensure everything compiles.
2. If you changed API or DB code, run **Option B** (ib_partition tests) with Postgres and `DATABASE_URL` set.
3. Then run your full build (e.g. `cargo make cargo-docker-minimal -- build -p nico-admin-cli --release`).

---

## Quick start (recommended on Mac)

Use the **minimal** path. The workspace (including `nico-rpc`) needs **`protoc`** to compile `.proto` files, so you build a small image once (~2–5 min) that adds only Rust + protoc.

**First time only — build the minimal image:**

```bash
# From repo root
cd /path/to/bare-metal-manager-core

cargo make build-cargo-docker-image-minimal
```

**Then run Cargo as needed:**

```bash
# Build admin-cli (default command)
cargo make cargo-docker-minimal

# Or pass a custom cargo command (each argument after --; do not wrap in quotes)
cargo make cargo-docker-minimal -- build -p nico-admin-cli --release
cargo make cargo-docker-minimal -- check -p nico-api --no-default-features
```

Output binaries appear under `target/` in your repo as usual.

---

## How to use: Minimal path (`cargo-docker-minimal`)

**When to use:** Day-to-day builds and checks on macOS when you don’t need the full API test stack (PostgreSQL, TSS, etc.).

**What it uses:** The image `nico-build-minimal` (Rust 1.97 + `protoc`). You must build it once with `build-cargo-docker-image-minimal` (~2–5 min). The `nico-rpc` crate needs `protoc` and the Google well-known proto files (`libprotobuf-dev`), so the bare `rust:1.97.1-slim-bookworm` image is not enough for workspace builds.

### Usage

```bash
# Default: build admin-cli in release mode
cargo make cargo-docker-minimal

# Custom cargo command (pass each argument after --; do not wrap in quotes)
cargo make cargo-docker-minimal -- build -p nico-admin-cli --release
cargo make cargo-docker-minimal -- check -p nico-api --no-default-features
cargo make cargo-docker-minimal -- build -p nico-api --no-default-features
```

### What works

- Building **nico-admin-cli**.
- Building/checking **nico-api** with `--no-default-features` (avoids `tss-esapi` and other heavy deps).
- Other crates that don’t need PostgreSQL, protobuf, or TSS.

### What doesn’t work

- Full **nico-api** with default features (needs TSS/measured-boot stack).
- API tests that require a database (slim image has no PostgreSQL client libs). For those, use the full image path below or run tests elsewhere (e.g. CI).

---

## How to use: Full image path (`build-cargo-docker-image` + `cargo-docker`)

**When to use:** When you need the full environment (e.g. run API tests with PostgreSQL, or build with default features). On Apple Silicon the image build is slow (45+ minutes); prefer the minimal path for routine work.

### Step 1: Build the image (once)

```bash
cargo make build-cargo-docker-image
```

Or manually:

```bash
docker build -f dev/docker/Dockerfile.build-container-x86_64 -t nico-build-x86_64 dev/docker
```

On Apple Silicon this runs under emulation and can take a long time; step 7 (installing cargo tools) alone often takes 45+ minutes.

### Step 2: Run Cargo in the full container

```bash
# Build admin-cli
cargo make cargo-docker -- build -p nico-admin-cli --release

# Run API tests (set DATABASE_URL if tests need Postgres)
export DATABASE_URL="postgres://nico_development:notforprod@host.docker.internal:5432/nico_development"
cargo make cargo-docker -- test -p nico-api ib_partition --no-fail-fast
```

For tests that need Postgres, start it first (e.g. `docker compose up -d postgresql`) and use `host.docker.internal` in `DATABASE_URL` so the container can reach the host’s Postgres.

---

## Command reference

| Goal | Command |
|------|--------|
| Build minimal image once (Rust + protoc; ~2–5 min) | `cargo make build-cargo-docker-image-minimal` |
| Build admin-cli (minimal) | `cargo make cargo-docker-minimal` |
| Build admin-cli release (minimal) | `cargo make cargo-docker-minimal -- build -p nico-admin-cli --release` |
| Check API without default features (minimal) | `cargo make cargo-docker-minimal -- check -p nico-api --no-default-features` |
| Postgres connectivity smoke test (quick; set DATABASE_URL) | `cargo make test-docker-postgres-smoke` or `cargo make cargo-docker-minimal -- test -p postgres-smoke-test` |
| Build full repo image (once, slow on Mac) | `cargo make build-cargo-docker-image` |
| Build admin-cli (full image) | `cargo make cargo-docker -- build -p nico-admin-cli --release` |
| Run API tests (full image; set DATABASE_URL) | `cargo make cargo-docker -- test -p nico-api ib_partition --no-fail-fast` |

---

## When to use which option

| Use case | Option |
|----------|--------|
| Build admin-cli on Mac | **cargo-docker-minimal** |
| Check or build API without TSS/measured-boot | **cargo-docker-minimal** with `--no-default-features` |
| Run API tests that need PostgreSQL | **build-cargo-docker-image** then **cargo-docker** (and set DATABASE_URL) |
| Build or test with full API default features | **build-cargo-docker-image** then **cargo-docker** |

---

## Tips and troubleshooting

1. **Cargo cache**  
   The tasks mount `CARGO_HOME` (e.g. `$HOME/.cargo`) into the container so dependency builds are cached on your Mac.

2. **sccache (faster repeat builds)**  
   The minimal image uses `sccache` and mounts `~/.sccache` (or `SCCACHE_DIR`) so compiled Rust artifacts are reused across runs. Rebuild the minimal image once (`cargo make build-cargo-docker-image-minimal`) to get sccache; then the second and later runs of `test -p nico-api ...` (and other cargo commands) are much faster.

3. **File ownership**  
   **`cargo-docker-minimal`** runs the container as root and runs `chown -R` on `/code` after each run, so `target/` is left owned by your user and "Permission denied" when writing to `target/` should not recur.  
   If you still see "Permission denied" (e.g. after using another Docker task that writes to the repo as root), from the repo root run:
   ```bash
   cargo make fix-target-permissions
   ```
   or manually: `sudo chown -R $(id -u):$(id -g) target` (or `.` for the whole repo).

4. **Postgres for API tests**  
   Start Postgres (e.g. `docker-compose up -d postgresql`; if the Loki plugin is missing, use `-f docker-compose.no-loki.yml` as well), then set:
   ```bash
   export DATABASE_URL="postgres://nico_development:notforprod@host.docker.internal:5432/nico_development"
   ```
   Use **`host.docker.internal`** (not `localhost`) so the container can reach Postgres on the host. `cargo-docker-minimal` passes `DATABASE_URL` into the container when set.

5. **Docker or tests hang**  
   - **DB tests:** If `DATABASE_URL` is unset or uses `localhost`, the container cannot reach Postgres and tests can hang. Set `DATABASE_URL` with `host.docker.internal` as the host (see above).  
   - **Stuck containers:** Stop them with `docker ps` then `docker stop <container_name>` or `docker rm -f <container_name>`.  
   - **Timeout:** Run tests with a time limit so they don’t hang indefinitely:
     - **Linux:** `timeout 300 cargo make cargo-docker-minimal -- test ...`
     - **macOS:** Install GNU coreutils then use `gtimeout`, or run without timeout and set `DATABASE_URL` so DB tests don’t hang:
       ```bash
       brew install coreutils   # one-time; provides gtimeout
       export DATABASE_URL="postgres://nico_development:notforprod@host.docker.internal:5432/nico_development"
       gtimeout 300 cargo make cargo-docker-minimal -- test -p nico-api ib_partition --no-default-features --no-fail-fast
       ```
     (300 seconds = 5 minutes; adjust as needed.)

6. **Apple Silicon (M1/M2/M3) and Colima**  
   The full build image is x86_64 and runs under emulation, so it is slow. Use **cargo-docker-minimal** for daily work; use the full image only when you need it.  
   **Colima:** For faster runs on M3, give the VM more resources and use the VZ runtime with virtiofs (see **Colima configuration** below).

7. **`protoc` required for workspace builds**  
   The `nico-rpc` crate compiles `.proto` files and needs the Protocol Buffers compiler (`protoc`). The minimal image adds only that; the full image includes it as well.

8. **Running without cargo-make**  
   You can run the same `docker run` locally. The minimal variant (after building `nico-build-minimal` once); add `-e DATABASE_URL="$DATABASE_URL"` when running tests that need the DB:
   ```bash
   docker run --rm \
     -v "$(pwd)":/code \
     -v "${CARGO_HOME:-$HOME/.cargo}":/cargo \
     -w /code \
     -u "$(id -u):$(id -g)" \
     -e CARGO_HOME=/cargo \
     ${DATABASE_URL:+-e DATABASE_URL="$DATABASE_URL"} \
     nico-build-minimal \
     cargo build -p nico-admin-cli --release
   ```
