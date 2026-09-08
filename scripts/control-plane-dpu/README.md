# DPU Initial Provisioning — User Guide

This toolchain provisions a BlueField DPU from scratch on a site controller host.
It is designed to be run **manually** by an operator from the host's BMC remote console.

> **Note:** This is for *initial DPU install* only. For firmware upgrades on
> already-running deployments, use the upgrade toolchain in
> [`upgrade/README.md`](upgrade/README.md), which preserves the DPU's existing
> configuration. The host has no network at this stage; networking
> is established as part of the provisioning process.

---

## Overview of steps

```text
[Build machine]   build-dpu-install-iso.sh   →  dpu_install_<ver>.iso
[Site controller] mount ISO, run install.sh  →  copies scripts and artifacts to /var/lib/dpu-install/<ver>/
[Site controller] provision-dpu.sh           →  flashes BFB, deploys HBN  →  power cycle
[Site controller] post-power-cycle.sh        →  verifies HBN container, configures netplan
                  (ISO not needed after install.sh — everything runs from the working directory)
```

---

## Part 1 — Build the install ISO

Run these steps on your **build machine** (Linux or macOS), not on the site controller.

### Prerequisites

The following tools must be installed on the build machine:

| Tool | Purpose |
|------|---------|
| `yq` (mikefarah v4) | Read site config YAML |
| `gomplate` | Render config templates |
| `wget` | Download BFB and DOCA host package |
| `docker` | Pull and save HBN container image |
| `curl`, `jq` | Download HBN config bundle from NGC |
| `xxd` | Decode NGC's base64 SHA256 hashes for verification |
| `zip`, `gzip` | Package artifacts |
| `sha256sum` / `shasum` | Verify downloaded files |
| `mkisofs` (Linux) or `xorrisofs` (macOS) | Build ISO |

Install on Ubuntu:
```bash
# yq (mikefarah v4) — do NOT use apt-get install yq, that installs the wrong one
sudo wget -qO /usr/local/bin/yq https://github.com/mikefarah/yq/releases/latest/download/yq_linux_amd64
sudo chmod +x /usr/local/bin/yq

# gomplate
sudo wget -qO /usr/local/bin/gomplate https://github.com/hairyhenderson/gomplate/releases/latest/download/gomplate_linux-amd64
sudo chmod +x /usr/local/bin/gomplate

sudo apt-get install wget curl jq zip gzip genisoimage
```

Install on macOS:
```bash
brew install yq gomplate wget curl jq zip xorriso
```

---

### Step 1 — Prepare the site config

Copy `site-sample.yaml` and fill in the values for your site.

Required fields: `datacenterAsn`, `siteControllerRoutesAsn`, `bgpAsnStart`, `siteControllerMtuSize`,
`forgeDpuLoopbackPrefix`, `forgeServiceVipPrefix`, `forgeControlPlanePrefix`, `nameServer`,
`ubuntuPasswordHash`, and `siteControllerNodes`.

Optional: the entire `fnn` block (only needed for FNN/SMN networking mode). When present,
`fnn.controlPlaneVni`, `fnn.commonManagedNodeBmcRouteTarget`, `fnn.commonSiteControllerRouteTarget`,
and `fnn.commonAdminNetworkTarget` are required; `fnn.vpcVrfLoopbackPrefix` and
`fnn.routeTargetsToImport` are optional.

```yaml
# yaml-language-server: $schema=
datacenterAsn: 4266030000
siteControllerRoutesAsn: 4266030000
bgpAsnStart: 4244766890

siteControllerMtuSize: 1500

fnn:
  controlPlaneVni: 60020
  # vpcVrfLoopbackPrefix: 10.255.248.0/21   # optional
  commonManagedNodeBmcRouteTarget: 900
  commonSiteControllerRouteTarget: 50100
  commonAdminNetworkTarget: 50400
  # Optional: additional EVPN route-targets to import (e.g. jumphosts, UFM, tenants).
  # routeTargetsToImport:
  #   datacenterAsn:101: {}   # Jumphosts
  #   datacenterAsn:1002: {}  # UFM

forgeDpuLoopbackPrefix: 7.243.97.64/26
forgeServiceVipPrefix: 7.243.86.224/27
forgeControlPlanePrefix: 7.243.86.192/27

nameServer: 8.8.8.8

# SHA-512 password hash for the ubuntu account on the DPU.
# Generate with: openssl passwd -6 'yourpassword'
ubuntuPasswordHash: "$6$rounds=4096$examplesalt$hashedvalue"

siteControllerNodes:
  - hostName: control223-cno1-cp1-jhb01
    mac: aa:bb:cc:dd:ee:01        # BlueField p0 MAC address
    nodeId: 1
  - hostName: control224-cno1-cp1-jhb01
    mac: aa:bb:cc:dd:ee:02
    nodeId: 2
  - hostName: control225-cno1-cp1-jhb01
    mac: aa:bb:cc:dd:ee:03
    nodeId: 3
```

The `mac` field is the BlueField **p0** MAC address for each node. If you do not know
it yet, you can use a placeholder (`aa:aa:aa:aa:aa:aa`) — `post-power-cycle.sh` will
detect and apply the real MAC automatically at the end of provisioning.

---

### Step 2 — Build the ISO

**Option A — Single command (download + build in one go):**

```bash
./build-dpu-install-iso.sh \
  --control-plane-config site-sample.yaml \
  --download-artifacts \
  --doca-version      3.2.2 \
  --bfb-build         125 \
  --bfb-release       26.02 \
  --hbn-version       3.2.2 \
  --hbn-container-tag 3.2.2-doca3.2.2 \
  --doca-host-url https://www.mellanox.com/downloads/DOCA/DOCA_v2.10.0/host/doca-host_2.10.0-093000-25.01-ubuntu2404_amd64.deb \
  --rshim-url     https://github.com/Mellanox/rshim-user-space/releases/download/rshim-2.3.1/rshim_2.3.1_amd64.deb \
  --libfuse2-url  http://archive.ubuntu.com/ubuntu/pool/universe/f/fuse/libfuse2t64_2.9.9-8.1build1_amd64.deb \
  --output-dir    ./output
```

**Option B — Download artifacts first, then build (useful to separate slow downloads from the build):**

```bash
# Step 2a: download artifacts into a local directory
mkdir -p ./artifacts
cd ./artifacts
../download-build-dpu-artifacts.sh \
  --doca-version      3.2.2 \
  --bfb-build         125 \
  --bfb-release       26.02 \
  --hbn-container-tag 3.2.2-doca3.2.2 \
  --doca-host-url https://www.mellanox.com/downloads/DOCA/DOCA_v2.10.0/host/doca-host_2.10.0-093000-25.01-ubuntu2404_amd64.deb \
  --rshim-url     https://github.com/Mellanox/rshim-user-space/releases/download/rshim-2.3.1/rshim_2.3.1_amd64.deb \
  --libfuse2-url  http://archive.ubuntu.com/ubuntu/pool/universe/f/fuse/libfuse2t64_2.9.9-8.1build1_amd64.deb
cd ..

# Step 2b: build ISO using pre-downloaded artifacts
./build-dpu-install-iso.sh \
  --control-plane-config site-sample.yaml \
  --doca-version  3.2.2 \
  --hbn-version   3.2.2 \
  --artifacts-dir ./artifacts \
  --output-dir    ./output
```

Option B is recommended when you need to build multiple ISOs for the same DOCA/HBN
version (e.g. different sites) — you download once and reuse the artifacts.

**Output:**

```text
output/
  dpu_install_3.2.2_3.2.2.iso
  dpu_install_3.2.2_3.2.2.zip
```

The ISO and ZIP have identical contents. Use the ISO for virtual media mount via BMC;
use the ZIP if you need to extract files directly onto the host filesystem.

---

### ISO contents

```text
dpu_install_3.2.2_3.2.2.iso
├── install.sh                    # Phase 1: run first
├── post-power-cycle.sh               # Phase 2: run after power cycle
├── setup_netplan.sh                  # Called automatically by post-power-cycle.sh
├── dpuinstall.sh                     # Called by provision-dpu.sh
├── bf.cfg.template                   # SSH key injection template
├── dpu_fw_version.cfg                # DOCA and HBN version info
├── doca_hbn_versions.cfg             # Paths inside the HBN config zip
├── bf-bundle-3.2.2-125_26.02_ubuntu-22.04_prod.bfb.gz
├── doca_hbn.tar.gz                   # HBN container image (linux/arm64)
├── doca_container_configs.zip.gz     # HBN scripts and configs bundle
├── doca-host_2.10.0-...amd64.deb    # DOCA host package
├── rshim_2.3.1_amd64.deb            # rshim driver package
├── libfuse2t64_2.9.9-..._amd64.deb  # rshim dependency (requires libc6 pre-installed)
└── servers/
    ├── control223-cno1-cp1-jhb01/
    │   ├── startup.yaml              # HBN DPU config for this node
    │   └── 99_config.yaml           # Host netplan for this node
    ├── control224-cno1-cp1-jhb01/
    │   ├── startup.yaml
    │   └── 99_config.yaml
    └── control225-cno1-cp1-jhb01/
        ├── startup.yaml
        └── 99_config.yaml
```

---

## Part 2 — Provision the site controller

Run these steps **on each site controller host** via its BMC remote console.

### Prerequisites

- OS: **Ubuntu 24.04 only** — no other OS version is supported. The check is enforced
  automatically by `install.sh`; use `--skip-os-check` only if the host has already
  been validated.
- Access: BMC remote console (IPMI/iDRAC/iLO)
- The host has **no network connectivity** at this stage — that is expected
- `libc6` must be installed — it is a dependency of `libfuse2t64`, which in turn is
  required by `rshim`. A clean Ubuntu 24.04 install includes `libc6` by default.

  Verify before running `install.sh`:

  ```bash
  dpkg -s libc6 | grep Status
  # Expected: Status: install ok installed
  ```

  If it is missing (e.g. on a stripped-down image), install it before proceeding:

  ```bash
  apt-get install libc6
  ```

---

### Step 1 — Transfer the ISO to the host

Since there is no network, use one of:

- **BMC virtual media** — mount the ISO directly from your workstation via the BMC UI
- **USB drive** — copy the ISO to a USB stick and insert it into the host

For virtual media, most BMC interfaces (iDRAC, iLO, IPMI) allow mounting a remote ISO.
Once mounted it appears as a CD-ROM device, e.g. `/dev/sr0`.

---

### Step 2 — Mount the ISO

From the BMC remote console, logged in as root:

```bash
mkdir -p /mnt/dpu-install
mount -o ro,loop /dev/sr0 /mnt/dpu-install
```

Or if you transferred the ISO file directly to the host:

```bash
mount -o ro,loop /path/to/dpu_install_3.2.2_3.2.2.iso /mnt/dpu-install
```

Verify the mount:

```bash
ls /mnt/dpu-install
# Expected: install.sh  post-power-cycle.sh  servers/  ...
```

---

### Step 3 — Run install.sh

Run `install.sh` from the ISO. This installs scripts and artifacts into a persistent
working directory on the host. It only needs to be run once per host.

```bash
/mnt/dpu-install/install.sh
```

This script will:

1. Verify the OS is Ubuntu 24.04
2. Install `libfuse2t64` from the ISO (rshim dependency; requires `libc6` pre-installed)
3. Install `rshim` from the ISO
4. Install `doca-host` from the ISO
5. Create the working directory at `/var/lib/dpu-install/3.2.2_3.2.2/`
6. Copy all scripts, artifacts, and per-node configs into the working directory

---

### Step 4 — Run provision-dpu.sh

The ISO is no longer needed after install.sh completes. Run `provision-dpu.sh` directly
from the working directory, specifying the hostname of **this specific host**:

```bash
/var/lib/dpu-install/3.2.2_3.2.2/provision-dpu.sh --server-name control223-cno1-cp1-jhb01
```

This script will:

1. Validate the server name against the installed configs
2. Flash the BFB firmware onto the DPU (~10–15 minutes)
3. Deploy the HBN container on the DPU
4. Prompt you to confirm a power cycle

At the power cycle prompt:

```text
Press Enter to trigger power cycle via ipmitool, or Ctrl+C to abort and power cycle manually:
```

Press **Enter** to let the script trigger it automatically, or **Ctrl+C** and power cycle
via the BMC UI if you prefer manual control.

> **Resuming after failure:** Re-run the same `provision-dpu.sh` command from the
> working directory. Touchfiles in `/var/lib/dpu-install/3.2.2_3.2.2/touchfiles/`
> record completed steps so the script resumes from where it left off.

---

### Step 5 — Wait for the host to come back up

After the power cycle the host will reboot. Reconnect via the BMC remote console and
wait for the OS to fully boot before proceeding.

---

### Step 6 — Run post-power-cycle.sh

Run directly from the working directory:

```bash
/var/lib/dpu-install/3.2.2_3.2.2/post-power-cycle.sh --server-name control223-cno1-cp1-jhb01
```

This script will:

1. Verify the HBN container is running on the DPU
2. Detect the BlueField p0 MAC address from the host
3. Apply the correct netplan configuration to bring up the host-DPU network interface

On success you will see:

```text
DPU provisioning complete
```

The host now has network connectivity through the DPU.

---

## Repeating for each node

Repeat **Part 2** (Steps 1–6) for each site controller host, substituting its hostname:

```bash
# install.sh only needs to run once — skip if already done on this host

# Node 2
/var/lib/dpu-install/3.2.2_3.2.2/provision-dpu.sh    --server-name control224-cno1-cp1-jhb01
# (power cycle)
/var/lib/dpu-install/3.2.2_3.2.2/post-power-cycle.sh --server-name control224-cno1-cp1-jhb01

# Node 3
/var/lib/dpu-install/3.2.2_3.2.2/provision-dpu.sh    --server-name control225-cno1-cp1-jhb01
# (power cycle)
/var/lib/dpu-install/3.2.2_3.2.2/post-power-cycle.sh --server-name control225-cno1-cp1-jhb01
```

The same ISO is used for all nodes — each `--server-name` selects the correct
per-node config from the `servers/` folder.

---

## Recovering from failures

`provision-dpu.sh` uses touchfiles to record completed steps so it can resume from
where it left off after a failure. You do not need to start over — just fix the
problem and re-run the same command.

### Touchfile locations

Touchfiles live under the working directory:

```text
/var/lib/dpu-install/<version>/touchfiles/
  bfbupdated          — BFB flash complete
  hbnconfigstaged     — startup.yaml copied to dpucfg/
  hbnsetupa           — HBN container image transferred and DPU rebooted
  hbndeployed         — HBN kubelet manifest installed
```

When `provision-dpu.sh` is re-run, each step checks for its touchfile and skips if
already done.

### How to resume after a failure

```bash
# Re-run provision-dpu.sh from the working directory — it resumes automatically
/var/lib/dpu-install/<version>/provision-dpu.sh --server-name <hostname>
```

Before re-running, restore the rshim and tmfifo environment if the script exited
mid-way (cleanup() stops rshim on exit):

```bash
systemctl enable rshim && systemctl start rshim
ip addr add 192.168.100.1/26 dev tmfifo_net0 2>/dev/null || true
ping -c 2 192.168.100.2   # confirm DPU is reachable before re-running
```

### Forcing a step to re-run

Delete the relevant touchfile and re-run:

```bash
# Example: force HBN deployment to re-run
rm /var/lib/dpu-install/<version>/touchfiles/hbnsetupa
rm /var/lib/dpu-install/<version>/touchfiles/hbndeployed
/var/lib/dpu-install/<version>/provision-dpu.sh --server-name <hostname>
```

### SCP of doca_hbn.tar.gz stalls (common)

This is a known issue caused by the **tmfifo interface** (`tmfifo_net0`) — a virtual
USB ethernet created by the RSHIM driver with ~1–5 Mbps throughput. It was designed
for management traffic, not bulk file transfer. Transferring the multi-GB HBN container
image over it is slow, and if the DPU is still busy with post-boot initialisation when
the transfer starts, its receive buffers fill up and the transfer stalls indefinitely.

The scripts mitigate this automatically:
- A **20-second delay** is inserted after the DPU comes online before any file transfer
  begins, giving the DPU time to finish its boot activity
- SSH keepalives (`ServerAliveInterval=30`, `ServerAliveCountMax=3`) detect a stalled
  connection and fail it within 90 seconds instead of hanging forever
- `dpu_scp` **retries up to 3 times** with a 60-second gap between attempts — by the
  second attempt the DPU has settled and the transfer typically succeeds quickly

If the transfer still stalls and exhausts all retries:

```bash
# Re-run provision-dpu.sh — it will retry the transfer from scratch
/var/lib/dpu-install/<version>/provision-dpu.sh --server-name <hostname>
```

If `hbnsetupa` touchfile was not yet created when the failure occurred, the script
will re-enter Phase A, detect that `/var/lib/hbn` already exists on the DPU (meaning
`hbn-dpu-setup.sh` already ran), skip the heavy setup steps, and only redo the file
transfer.

---

## Troubleshooting

### BFB download failed (404)

Verify `--bfb-release` is correct (e.g. `26.02`, not `26.0`) and that `--bfb-build`
matches a real build for that DOCA version.

### doca-host install failed — missing dependencies

The host has no network at install time so missing dependencies cannot be fetched.
Ensure the host OS is a clean Ubuntu 24.04 install — required libraries are present
by default. If you have added/removed packages, a fresh OS install is recommended.

### bfb-install not found after doca-host install

The `doca-host` package may be corrupted or the wrong architecture. Verify the `.deb`
filename ends in `amd64.deb` and was downloaded for Ubuntu 24.04.

### bfb-install warning: pv command not found

This warning is expected and harmless:

```text
Warn: pv command not found. Continue without showing bfb progress.
```

`pv` is used only to display a progress bar during BFB flashing. It is not installed
on a clean Ubuntu 24.04 system and is not included in the ISO. The BFB install
completes normally without it.

### DPU not ready after BFB install (timeout)

The BFB install waits up to 30 minutes for the DPU to signal readiness. If it times
out, check the rshim console for errors:

```bash
cat /dev/rshim0/misc
```

### post-power-cycle.sh: touchfile not found

The power cycle happened before `provision-dpu.sh` completed the HBN deployment step.
Re-run `provision-dpu.sh` — touchfiles will skip already-completed steps and resume
from where it left off. See the **Recovering from failures** section above.

### BlueField p0 interface not found (netplan)

`lshw` did not detect a BlueField network interface. Verify the DPU is seated correctly
and that the BFB flash completed successfully. Re-running `post-power-cycle.sh` after
confirming the HBN container is running will retry the detection.
