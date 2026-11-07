# DPU Firmware Upgrade — User Guide

This toolchain upgrades the firmware (BFB) and HBN stack of a BlueField DPU
that is **already provisioned and running**, while preserving its existing
configuration. It is designed to be run **manually** by an operator from the
host's BMC remote console, because host networking (which runs through the
DPU) goes down during the upgrade.

The upgrade ISO is **generic**: it carries no site config and no per-node
files, so the same ISO upgrades any BlueField DPU running the HBN stack, on
any host. For initial provisioning, see [`../README.md`](../README.md); for
the design rationale behind this toolchain, see
[`design-discussion.md`](design-discussion.md).

## How it differs from initial provisioning

| | Initial install | Upgrade |
|---|---|---|
| Site config YAML | Required at build time | Not used |
| `startup.yaml` | Generated per node from templates | **Backed up from the running DPU** and redeployed |
| Host netplan | Generated and applied | **Untouched** — the p0 MAC is validated instead |
| Per-node `servers/` dir | In the ISO | Not present |
| DPU login before flashing | Not needed (DPU is blank) | Needed for the backup — SSH key or password |

---

## Overview of steps

```text
[Build machine]   build-dpu-upgrade-iso.sh     →  dpu_upgrade_<ver>.iso
[Host]            mount ISO, upgrade-install.sh →  copies scripts and artifacts to /var/lib/dpu-upgrade/<ver>/
[Host]            upgrade-dpu-fw.sh            →  backs up startup.yaml + p0 MAC, flashes BFB,
                                                   redeploys HBN with the saved config  →  power cycle
[Host]            upgrade-post-power-cycle.sh  →  verifies HBN container, verifies p0 MAC unchanged
                  (ISO not needed after upgrade-install.sh)
```

---

## Part 1 — Build the upgrade ISO

Run on your **build machine** (Linux or macOS). The tool prerequisites are the
same as for the install ISO (see [`../README.md`](../README.md)), except that
`yq` is not needed — there is no site config.

Flashing wipes the DPU OS, so the new image still needs a password for its
`ubuntu` account. Generate the hash with:

```bash
openssl passwd -6 'yourpassword'
```

**Option A — single command (download + build in one go):**

```bash
./build-dpu-upgrade-iso.sh \
  --ubuntu-password-hash '$6$rounds=4096$examplesalt$hashedvalue' \
  --download-artifacts \
  --doca-version      3.2.2 \
  --bfb-build         125 \
  --bfb-release       26.02 \
  --hbn-version       3.2.2 \
  --hbn-container-tag 3.2.2-doca3.2.2 \
  --doca-host-url https://www.mellanox.com/downloads/DOCA/DOCA_v2.10.0/host/doca-host_2.10.0-093000-25.01-ubuntu2404_amd64.deb \
  --rshim-url     https://github.com/Mellanox/rshim-user-space/releases/download/rshim-2.3.1/rshim_2.3.1_amd64.deb \
  --libfuse2-url  https://archive.ubuntu.com/ubuntu/pool/universe/f/fuse/libfuse2t64_2.9.9-8.1build1_amd64.deb \
  --output-dir    ./output
```

Match the `--doca-host-url` package to your target `--doca-version` per
NVIDIA's DOCA compatibility guidance — the installer flashes the BFB and
installs the host tooling as one coherent set. The versions shown are from a
validated lab combination; substitute the pairing published for your release.

**Option B — reuse pre-downloaded artifacts** (from
`../download-build-dpu-artifacts.sh`, exactly as for the install ISO — the
same artifacts directory works for both):

```bash
./build-dpu-upgrade-iso.sh \
  --ubuntu-password-hash '$6$rounds=4096$examplesalt$hashedvalue' \
  --artifacts-dir ./artifacts \
  --output-dir    ./output
```

**Output:**

```text
output/
  dpu_upgrade_3.2.2_3.2.2.iso
  dpu_upgrade_3.2.2_3.2.2.zip
```

---

## Part 2 — Upgrade the DPU

Run on the host via its BMC remote console, as root. Host prerequisites are
the same as for initial provisioning (Ubuntu 24.04, `libc6`; see
[`../README.md`](../README.md)).

### Step 1 — Mount the ISO and run upgrade-install.sh

```bash
mkdir -p /mnt/dpu-upgrade
mount -o ro,loop /dev/sr0 /mnt/dpu-upgrade
/mnt/dpu-upgrade/upgrade-install.sh
```

This installs scripts and artifacts into `/var/lib/dpu-upgrade/<version>/`.
The ISO is no longer needed afterwards.

The host packages (`libfuse2t64`, `rshim`, `doca-host`) are installed from
the ISO if missing, and **upgraded** if the ISO carries a newer version — so
`bfb-install` and the rshim driver match the firmware being flashed. Packages
at the same or a newer version are left alone. If a package upgrade hits a
missing dependency, it is resolved automatically with `apt-get`
(the host still has network at this point). To keep the already-installed
versions instead, re-run with `--skip-package-upgrade` (missing packages are
still installed from the ISO).

### Step 2 — Run upgrade-dpu-fw.sh

Pick the row that matches this host and run the command. Every command below
runs from the working directory (`/var/lib/dpu-upgrade/<version>/`).

| Your situation | Run `upgrade-dpu-fw.sh` with |
|---|---|
| **Standard command** — DPU provisioned by this toolchain | `--ssh-key /root/.dpu_provision/dpu_provision_ed25519` |
| Password login only (no key) | `--auth password --dpu-user ubuntu` |
| Login with a different SSH key | `--ssh-key <path-to-that-key>` |
| Live `startup.yaml` is bad — deploy a known-good one | `--startup-yaml-file <known-good-config>` |
| No way to log in to the DPU | `--startup-yaml-file <known-good-config>` |
| DPU hardware was replaced | `--startup-yaml-file /var/lib/dpu-upgrade/<version>/startup-configs/<name>` |

The **standard command** (first row) is the normal path for hosts provisioned
by this toolchain; the rest of this page refers back to it by that name. If
you are unsure about the credentials, run it anyway: the script checks them
**before changing anything** and, on failure, prints the exact command to run
next.

The script then:

1. Saves the DPU's `startup.yaml` and BlueField p0 MAC address to
   `/var/lib/dpu-upgrade/<version>/backup/` (with `--startup-yaml-file`,
   your file is used instead of fetching one)
2. Flashes the BFB firmware (~10–15 minutes)
3. Redeploys the HBN container with the saved `startup.yaml`
4. Prompts you to confirm a power cycle

Nothing is flashed until the backup has succeeded.

#### What the upgraded DPU comes back with

- **Its previous `startup.yaml`** (or the file you supplied).
- **Its previous ubuntu password**, on hosts provisioned by this toolchain.
  The live password is read from the DPU during the backup (so a password
  rotated since initial provisioning is preserved too); when that read is not
  possible, the hash recorded at initial provisioning is used and the script
  says so. Add `--replace-ubuntu-password` to switch to the password baked
  into this ISO. Hosts without previous provisioning credentials always get
  this ISO's password.
- **A working provisioning SSH key** at
  `/root/.dpu_provision/dpu_provision_ed25519` — the existing one if present,
  a new one otherwise. Add `--regenerate-dpu-credentials` to rotate it (this
  also switches the DPU to this ISO's password).
- The host's **netplan is never modified**; Step 3 verifies the DPU's MAC is
  unchanged so the existing netplan keeps working.

#### If the standard command fails

- **Broken or missing credential files on the host** — the script stops
  before changing anything. Recover with one command (you are prompted for
  the DPU password on the console):

  ```bash
  /var/lib/dpu-upgrade/<version>/upgrade-dpu-fw.sh \
    --auth password --dpu-user ubuntu --regenerate-dpu-credentials
  ```

  Note: after this recovery the DPU's password becomes the one baked into
  this ISO.

- **Key is fine on the host but the DPU rejects it** — the run stops at the
  (read-only) config fetch. Re-run with `--auth password --dpu-user ubuntu`;
  key login works again after the upgrade.

- **No login works at all** — reflash with a saved config:
  `--startup-yaml-file <saved-startup.yaml>`. Any saved copy works: an
  earlier run's `backup/startup.yaml`, or a config embedded in the ISO (see
  *DPU replacement* below). If no saved copy exists anywhere, the running
  configuration cannot be recovered — fall back to a fresh install.

A wrong password is harmless: ssh prompts up to 3 times, then the run aborts
with nothing modified — just re-run. To test the password without starting
the upgrade:

```bash
ssh -o PubkeyAuthentication=no -o PreferredAuthentications=password,keyboard-interactive ubuntu@192.168.100.2 true
```

(Needs rshim/tmfifo up: `systemctl start rshim` and
`ip addr add 192.168.100.1/26 dev tmfifo_net0` if the upgrade has not run
yet on this host.)

#### DPU replacement

A blank replacement DPU leaves the host with no network, so the saved
`startup.yaml` must be embedded in the ISO at build time:

```bash
./build-dpu-upgrade-iso.sh ... --include-startup-yaml /path/to/saved-configs/
```

After `upgrade-install.sh`, the configs are available under
`/var/lib/dpu-upgrade/<version>/startup-configs/` — point
`--startup-yaml-file` at the right one. After the upgrade, update the MAC in
the host's netplan config to the new DPU's p0 MAC (`netplan generate &&
netplan apply`); this toolchain never edits netplan for you.
`upgrade-post-power-cycle.sh` warns if no netplan file references the
detected MAC, so a forgotten update is caught rather than silently passed.

#### Options for non-default DPUs

- `--dpu-user USER` — login user on the running DPU (default: `root`). Stock
  BlueField Ubuntu images disable root password login, so with
  `--auth password` use `--dpu-user ubuntu`.
- `--dpu-host HOST` — address of the running DPU for the backup step
  (default: `192.168.100.2` over tmfifo; rshim and the tmfifo interface are
  brought up automatically). After flashing, the DPU is always reached at
  `192.168.100.2` over tmfifo.
- `--startup-yaml-path PATH` — location of the live HBN config on the DPU
  (default: `/var/lib/hbn/etc/nvue.d/startup.yaml`).
- `--replace-ubuntu-password` — switch the DPU to this ISO's ubuntu password
  instead of keeping its existing one.
- `--regenerate-dpu-credentials` — rotate the provisioning SSH key (implies
  this ISO's password). Also the remedy for broken credential files.

For the reasoning behind these behaviors (credential convergence, recovery
design, fleet considerations), see
[`design-discussion.md`](design-discussion.md).

### Step 3 — After the power cycle, run upgrade-post-power-cycle.sh

Reconnect via the BMC console once the host has booted:

```bash
/var/lib/dpu-upgrade/<version>/upgrade-post-power-cycle.sh
```

This verifies that the HBN container is running, that the BlueField p0 MAC
address is **unchanged** from before the upgrade, and that the host netplan
actually **references that MAC** (so a DPU swapped before the upgrade cannot
produce a false all-clear). No netplan is ever written. On success you will
see:

```text
DPU firmware upgrade complete
```

If the MAC **did** change (e.g. the DPU hardware was replaced rather than
upgraded), the script fails and prints both MACs. Update the MAC in your
netplan config (e.g. `/etc/netplan/99_config.yaml`), run
`netplan generate && netplan apply`, acknowledge the new MAC in
`backup/p0_mac.replaced`, and re-run the script — it tells you the exact
commands. The pre-flash record in `backup/p0_mac` is never overwritten, so it
stays available for audit.

---

## Recovering from failures

The same touchfile mechanism as initial provisioning applies (see
[`../README.md`](../README.md)): re-run `upgrade-dpu-fw.sh` with the same
arguments and completed steps are skipped. One additional touchfile exists:

```text
/var/lib/dpu-upgrade/<version>/touchfiles/
  upgradebackup       — startup.yaml and p0 MAC saved from the running DPU
  bfbupdated          — BFB flash complete
  hbnconfigstaged     — saved startup.yaml copied to dpucfg/
  hbnsetupa           — HBN container image transferred and DPU rebooted
  hbndeployed         — HBN kubelet manifest installed
```

`upgradebackup` matters after a mid-upgrade failure: once the DPU has been
flashed, its old config is gone, so the backup step must **not** re-run — the
touchfile guarantees the saved files from the original backup are reused.
Never delete `touchfiles/upgradebackup` or the `backup/` directory after
flashing has started.

The tmfifo transfer stalls, rshim restore steps, and other troubleshooting
notes from [`../README.md`](../README.md) apply unchanged.
