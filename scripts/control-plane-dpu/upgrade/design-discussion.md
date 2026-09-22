# DPU Firmware Upgrade — Design Discussion

This document records the design decisions behind the upgrade toolchain and
the reasoning for them. Operators should read [`README.md`](README.md); this
page is for maintainers reviewing changes and for QA deriving test cases.

## Requirements

1. **Do not modify the QA'd install scripts.** The on-server provisioning
   scripts (`dpuinstall.sh`, `install.sh`, `provision-dpu.sh`,
   `post-power-cycle.sh`, `setup_netplan.sh`) are validated on hardware and
   must not change. The upgrade flow may reuse them as-is.
2. **Preserve the DPU's configuration.** Unlike initial install, the upgrade
   must save the live HBN `startup.yaml` before flashing and redeploy it
   after.
3. **Never touch host netplan.** The existing host network config stays;
   instead, validate after the upgrade that the BlueField p0 MAC did not
   change (the netplan matches the DPU by MAC).
4. **Generic and fleet-wide.** One upgrade ISO, no site config, no per-node
   files. The same artifact must handle a fleet of DPUs in mixed states:
   provisioned by this toolchain, provisioned by other means (password only,
   or password + own SSH key), corrupted credentials, corrupted
   `startup.yaml`, replaced hardware.

## Decisions and rationale

### Reuse of dpuinstall.sh by sourcing

`dpuinstall.sh` has a `BASH_SOURCE` guard, so sourcing it loads only its
functions (`start_rshim`, `install_bfb`, `setup_hbn`, `check_hbn_container`,
the `dpu_ssh`/`dpu_scp` helpers with their retry/stall mitigations). The
upgrade drivers (`upgrade-dpu-fw.sh`, `upgrade-post-power-cycle.sh`)
orchestrate those functions in a different order and skip the netplan step —
zero changes to the QA'd file. Before invoking the reused functions, the
driver switches to the exact shell-option state the install flow was QA'd
under (`set -eux`, no `nounset`/`pipefail`).

### Startup config source: exactly one of three

The DPU login exists **only** to fetch the live `startup.yaml`, so the config
source is the single per-host variable:

- `--ssh-key <path>` — fetch over SSH with a key.
- `--auth password` — fetch over SSH; ssh itself prompts interactively on the
  console. The password never appears on a command line, in a file, in `ps`,
  or in shell history — this is why `sshpass` was rejected. Consequence: the
  password cannot be pre-verified programmatically; it is *verified by use* —
  the fetch is the first DPU-touching step and is read-only, so a wrong
  password aborts before anything is modified.
- `--startup-yaml-file <path>` — no DPU login at all; deploy an
  operator-provided config. Covers corrupted `startup.yaml`, lost
  credentials, and replaced hardware.

### Credential convergence (the bf.cfg refresh)

Flashing wipes the DPU OS, so the new image's credentials come from the
prepared `bf.cfg`. The reused `dpu_ssh_prepare` (unchangeable) reuses an
existing prepared `bf.cfg` verbatim — which would silently flash the
*original install ISO's* password and settings. Three options were
considered:

- **A. Reuse as-is (status quo):** a fleet upgrade leaves mixed passwords
  (old on previously-installed hosts, new elsewhere) with no record of which
  is which. Rejected.
- **B. Always regenerate credentials:** converges the fleet but has a
  lockout trap — regenerating the key on a *resumed* run after the flash
  already succeeded invalidates the key baked into the flashed DPU.
  Rejected as the default.
- **C. Refresh the prepared bf.cfg from the upgrade ISO's template,
  re-injecting the existing key's public key (chosen):** settings converge,
  the key never changes mid-flow (no lockout), and `dpuinstall.sh` needs no
  change — `dpu_ssh_prepare` sees all four credential files present and uses
  the refreshed file. Guard: the refresh is skipped once the `bfbupdated`
  touchfile exists (the injected config is already on the DPU).

### Password policy: keep by default

Fleet operators should not have to re-socialize a new DPU password on every
firmware upgrade. The backup fetch also reads the DPU's **live** ubuntu
shadow hash (same SSH session — one password prompt; best-effort, needs root
or passwordless sudo), so a password rotated after initial provisioning is
preserved too. During the bf.cfg refresh the live hash is preferred; the
`ubuntu_PASSWORD=` line from the old prepared `bf.cfg` is the fallback.
`--replace-ubuntu-password` installs the upgrade ISO's hash instead.
Hosts without previous credentials, and any `--regenerate-dpu-credentials`
run, necessarily get the ISO's hash (there is no old one to keep). The ISO
build still requires `--ubuntu-password-hash` for exactly those cases.
(Hash lines are carried via `awk` — crypt hashes use only `[./$A-Za-z0-9]`,
so no metacharacter escaping issues.)

### Backup before any credential mutation

The main sequence runs the backup (config fetch + p0 MAC capture) **before**
`--regenerate-dpu-credentials` or the bf.cfg refresh can touch anything.
This makes "fetch with the old provisioning key, then rotate it" work in a
single invocation, and underpins the one-command recovery below.

### Fail-fast credential check and prescriptive recovery

`dpu_ssh_prepare` hard-errors on partial credential state, but only mid-flow.
The driver checks up front — before the backup, before rshim — and prints
the exact recovery command. Failure-point matrix (QA: each row is a test
case):

| Situation | Where it fails | Message directs to |
|---|---|---|
| Some of the 4 credential files missing | Early check, nothing touched | `--auth password --dpu-user ubuntu --regenerate-dpu-credentials` (one command: fetch by password → discard remnants → fresh credentials) |
| All 4 present, key file corrupt (`ssh-keygen -y` fails) | Early check, nothing touched | same as above |
| `--ssh-key` path missing/unreadable | Argument validation | same as above |
| Key intact but DPU rejects it | Backup fetch (read-only) | `--auth password --dpu-user ubuntu` — **without** regeneration: the host key is intact and is re-injected at flash |
| No login works at all | Backup fetch | `--startup-yaml-file <saved config>` (recovery reflash needs no DPU login) |
| No login and no saved config | — | Config unrecoverable; fresh install or out-of-band DPU password reset (out of scope) |

The four credential files: `/var/dpu_ssh_prepared`,
`/root/.dpu_provision/dpu_provision_ed25519`(+`.pub`),
`/root/.dpu_provision/bf.cfg`.

### DPU replacement: config must travel in the ISO

With a blank replacement DPU the host has **no network** (its uplink is the
DPU), so a saved `startup.yaml` cannot be fetched or copied in at run time.
`build-dpu-upgrade-iso.sh --include-startup-yaml <file-or-dir>` embeds saved
configs under `startup-configs/`; `upgrade-install.sh` copies them into the
working directory for use with `--startup-yaml-file`. Known caveats: the new
DPU has a different p0 MAC, so host netplan must be updated manually (the
toolchain never writes netplan). The before/after MAC comparison passes by
design in this scenario (the pre-flash capture already sees the new card), so
`upgrade-post-power-cycle.sh` additionally cross-checks that a file under
`/etc/netplan/` references the detected MAC and warns when none does —
catching a forgotten netplan update instead of reporting a false all-clear.

### Host package handling on upgrade

Unlike initial install (offline, presence-check only), the upgrade host
usually has network until the DPU is touched. `upgrade-install.sh` compares
versions: it upgrades a package when the ISO's `.deb` is newer (so
`bfb-install`/rshim match the firmware being flashed), retries dependency
failures once with `apt-get --fix-broken`, and offers
`--skip-package-upgrade` as the escape hatch.

### Miscellaneous

- **Working directory** is `/var/lib/dpu-upgrade/<ver>/` (separate namespace
  from `/var/lib/dpu-install/` so install and upgrade state cannot collide).
- **`touchfiles/upgradebackup`** guards the backup: once flashing has
  started, the old config is gone from the DPU, so the backup step must
  never re-run — the touchfile pins the originally saved files.
- **No usable `/dev/tty`** (e.g. a session without a controlling terminal)
  used to kill the scripts silently via SIGPIPE on the log-tee. The upgrade
  scripts keep the original stderr on fd 4 and fall back to it. The QA'd
  install scripts share the latent behavior but are always run from a real
  BMC console; per requirement 1 they were left unchanged.

## Validation

Unit tests live in [`../unit-tests/`](../unit-tests/) (`test_upgrade_*.sh`).
The flow was validated end-to-end — including credential reuse/refresh,
password keep/replace, fail-fast recovery messages, resume, MAC-changed
failure, and a DPU-replacement simulation (fake DPU offline until the flash
completes) — on an emulated Linux testbed (netns fake DPU, stubbed
rshim/bfb-install). The testbed is planned as a separate contribution; until
then, QA can derive test cases from the failure-point matrix above.
