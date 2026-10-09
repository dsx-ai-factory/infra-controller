<!--
SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
SPDX-License-Identifier: Apache-2.0
-->

# NICo CLI Installation Prompt (for coding agents)

This file is a prompt: paste it (or a link to it) into a coding agent that has shell access and the agent will install the NICo CLI (`nicocli`) for you. The agent will clone the repo, run the `make` target, handle common errors, and verify the install. If you are a human reading this, follow the steps yourself or use `cli/README.md` instead.

---

## Instructions to the agent

You are installing the NICo CLI (`nicocli`) for the user. Follow the steps below in order. Do not skip the verification step. If a step fails, consult the "Common failures" section before retrying; do not retry the same failing command more than twice without changing something.

### Success criteria

When you are done, all of the following must be true:

1. A binary named `nicocli` exists at a known path.
2. That path is on the user's `PATH` in a fresh shell (or the user has been told explicitly how to add it).
3. `nicocli --version` runs without error.
4. You have told the user the install path and version.

If any of those are not true, the task is not complete.

### Steps

1. Check prerequisites. Run:

   ```bash
   go version
   git --version
   make --version
   ```

   You need Go 1.26.4 or newer, plus working `git` and `make`. If any is missing or too old, stop and tell the user what to install. Do not attempt to install Go, git, or make yourself unless the user explicitly asks.

2. Pick a working directory the user owns. Default to `~/Developer/nicocli-install/`. Do not use `/tmp/` (some agent sandboxes block it, and some build systems treat it as ephemeral):

   ```bash
   WORKDIR="$HOME/Developer/nicocli-install"
   mkdir -p "$WORKDIR"
   cd "$WORKDIR"
   ```

3. Clone the upstream repo (shallow clone is fine; this is a build workspace, not a development checkout):

   ```bash
   git clone --depth 1 https://github.com/dsx-ai-factory/infra-controller.git
   cd infra-controller/rest-api
   ```

4. Build and install with the provided `make` target:

   ```bash
   make nico-cli
   ```

   This builds `nicocli` and installs it to `$(go env GOPATH)/bin/nicocli` by default. To install elsewhere (for example, system-wide), pass `INSTALL_DIR`:

   ```bash
   make nico-cli INSTALL_DIR="$HOME/.local/bin"
   # or, with sudo for a system path:
   sudo make nico-cli INSTALL_DIR=/usr/local/bin
   ```

   Prefer a user-owned directory over `/usr/local/bin` so you don't need `sudo`.

5. Verify the binary exists and reports a version. Use the absolute path first so you don't depend on `PATH`:

   ```bash
   "$(go env GOPATH)/bin/nicocli" --version
   ```

   Then verify it is on `PATH` in a fresh shell. If not, see "PATH not set" below.

6. Report back to the user with:

   - The absolute install path (`$(go env GOPATH)/bin/nicocli` or the `INSTALL_DIR` you used)
   - The version string printed by `nicocli --version`
   - Any `PATH` shell-rc edit the user still needs to make

### Common failures

- **`command not found: go`** — Go is not installed. Stop and tell the user to install Go 1.26.4 or newer from [https://go.dev/dl/](https://go.dev/dl/). Do not auto-install Go.

- **`go: go.mod requires go >= 1.26.4`** — Installed Go is too old. Stop and ask the user to upgrade.

- **`command not found: make`** — `make` is not installed. On macOS, suggest `xcode-select --install`. On Debian/Ubuntu, suggest `sudo apt install build-essential`. If the user cannot install `make`, fall back to a direct `go build`:

  ```bash
  go build -o "$(go env GOPATH)/bin/nicocli" ./cli/cmd/cli
  ```

- **`command not found: git`** — install `git` first. macOS: `xcode-select --install`. Linux: use the distro package manager.

- **PATH not set: `nicocli: command not found` after a successful build** — `$(go env GOPATH)/bin` is not on `PATH`. Tell the user to add it. Detect the shell first:

  ```bash
  echo "$SHELL"
  ```

  For zsh (default on macOS):

  ```bash
  echo 'export PATH="$(go env GOPATH)/bin:$PATH"' >> ~/.zshrc
  ```

  For bash:

  ```bash
  echo 'export PATH="$(go env GOPATH)/bin:$PATH"' >> ~/.bashrc
  ```

  For fish:

  ```fish
  fish_add_path "$(go env GOPATH)/bin"
  ```

  Tell the user to open a new shell or `source` the rc file before running `nicocli`.

- **`permission denied` writing to `/usr/local/bin` or another system path** — re-run with `sudo` or switch `INSTALL_DIR` to a user-owned directory (`~/.local/bin`, `~/bin`, etc.). Do not chmod the system directory.

- **`fatal: unable to access 'https://github.com/...': ...` or `go mod download` network errors** — confirm the user has internet access and (if behind a corporate proxy) that `HTTPS_PROXY` / `GOPROXY` are set. Surface the exact error to the user; do not retry indefinitely.

- **Repo not found at `dsx-ai-factory/infra-controller`** — confirm that the
  clone URL is [https://github.com/dsx-ai-factory/infra-controller.git](https://github.com/dsx-ai-factory/infra-controller.git). If the
  repository is unavailable, stop and surface the exact Git error.

- **`go: cannot find main module`** — you are not inside the cloned repo. Re-run `cd` into the cloned directory.

- **Build succeeds but `nicocli --version` prints nothing or hangs** — the binary may be partially built or running TUI mode. Confirm you are passing `--version` exactly, and run with a 10-second timeout:

  ```bash
  timeout 10 nicocli --version
  ```

  If it still hangs, surface the issue to the user.

### What not to do

- Do not modify any source files in the cloned repo.
- Do not create commits, branches, or PRs in the cloned repo — it is a build workspace, not a development checkout.
- Do not delete or overwrite the user's existing `~/.nico/config.yaml`.
- Do not install Go, git, or `make` automatically; ask the user instead.
- Do not retry the same failing command more than twice. After two failures, change strategy or surface the failure to the user.

### Cleanup

Once `nicocli --version` works, you may leave the cloned repo in place at `$WORKDIR` (so the user can rebuild later) or ask the user whether to remove it. Default: leave it in place.

---

## Where to send agents

To install via your agent, paste the URL of this file as a prompt. Example phrasings:

- "Install nicocli following the instructions at [https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/cli/INSTALL.md](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/cli/INSTALL.md)"
