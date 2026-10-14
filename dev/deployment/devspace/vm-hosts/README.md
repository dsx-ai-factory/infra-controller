# DevSpace with real zero-DPU target VMs

The opt-in `vm-hosts` profile inherits `full` and replaces machine-a-tron with
native KVM guests. A standalone `bmc-mock` controls each guest through libvirt.
The default `full` profile continues to use machine-a-tron.

This profile exercises discovery and OS provisioning on real guest operating
systems, with IPv4 networking. IPv6 experiments remain separate under
[issue 84](https://github.com/dsx-ai-factory/infra-controller/issues/84).
It does not emulate BlueField forwarding, DPU agents, GPU devices, or hardware
isolation.

## Requirements

Use an x86_64 or ARM64 Linux Docker host with `/dev/kvm`, Python 3.11 or later,
PyYAML, the normal DevSpace prerequisites, and a single-node kind cluster whose
provisioning services use IPv4. A physical host needs hardware virtualization;
a VM host additionally needs nested virtualization. The guest architecture is
always native: x86_64 uses Q35/OVMF and ARM64 uses virt/AAVMF.

Allow 4 GiB RAM, two vCPUs, and a 24 GiB sparse disk per target by default.
These are starting budgets, not established Scout minimum requirements. Allow
additional memory and disk space for the stack, builds, and QEMU.

Use a separate kind cluster or an empty site. Setup refuses Expected Machines
belonging to other targets; it does not migrate MAT inventory. Keep the site
database and Vault paired with the persistent guest and BMC state.

## Boot artifacts

Set `LOCAL_DEV_VM_HOSTS_BOOT_IMAGE` to an image containing `ipxe.efi`,
`scout.efi`, `scout.squashfs`, and `qcow-imager.efi` under `/x86_64/` or
`/aarch64/`, matching the Docker host. The PXE init container checks all four
files before copying them into the PXE volume. Local Docker images are loaded
into kind; other references must be pullable using the site's registry setup.

Use artifacts from a compatible revision and record their provenance. Rebuild
Scout when testing Scout changes. The production native build and packaging
tasks are in [the PXE tasks](../../../../pxe/Makefile.toml) and
[the root tasks](../../../../Makefile.toml).

### Building Scout for virtual targets

`build_boot_artifacts.py` stages the production Scout service and userspace with
Ubuntu's `linux-image-virtual` kernel in both boot stages. It excludes GPU
packages and repositories and does not alter the production profiles. On ARM64,
this also avoids the hardware profile's 64 KiB kernel, which cannot run on vfkit
CPUs without 64 KiB page support.

Build as root on native Linux with the prerequisites from
[the mkosi setup action](../../../../.github/actions/setup-mkosi-environment/action.yml).
Initialize the pinned `pxe/mkosi` submodule. Supply a real `forge-scout` Debian
package matching the host (`amd64` or `arm64`) and compatible iPXE and imaging
EFI files:

```bash
git submodule update --init pxe/mkosi
sudo python3 dev/deployment/devspace/vm-hosts/build_boot_artifacts.py \
  --scout-package /absolute/path/to/forge-scout.deb \
  --boot-dir /absolute/path/to/efi-inputs \
  --output /absolute/path/to/new-vm-boot

docker build \
  --build-arg BOOT_ARCH="$(uname -m)" \
  -f dev/deployment/devspace/vm-hosts/Dockerfile.boot-artifacts \
  -t nico-vm-hosts-boot:local /absolute/path/to/new-vm-boot
export LOCAL_DEV_VM_HOSTS_BOOT_IMAGE=nico-vm-hosts-boot:local
```

All three builder arguments are required. `--boot-dir` contains `ipxe.efi` and
`qcow-imager.efi` directly. `--output` must be new or empty and retains the mkosi
workspace under `build/`, payloads under the native architecture directory, and
provenance in `build-info.json`. Failed builds retain their workspace and cannot
be overwritten by another invocation. Downloads require network access. The
Dockerfile's `BOOT_ARCH` defaults to `x86_64`; use `aarch64` for ARM64 payloads.

## Deployment

From the repository root, with the normal DevSpace prerequisites prepared and
the intended kind context selected:

```bash
export LOCAL_DEV_VM_HOSTS_BOOT_IMAGE=nico-vm-hosts-boot:local
export LOCAL_DEV_VM_HOST_COUNT=2
devspace deploy -n nico-system --profile vm-hosts
```

`LOCAL_DEV_VM_HOSTS_BOOT_IMAGE` is required. `LOCAL_DEV_VM_HOST_COUNT` defaults
to `2` and accepts `1` or `2`. Setup builds `bmc-mock` instead of MAT, creates
persistent guests initially powered off, and registers zero-DPU Expected
Machines. NICo then controls discovery, Scout boot, security checks and
provisioning through real Redfish endpoints. TPM checks are disabled for these
virtual targets; Scout identity checks and BMC lockdown remain enabled.

The profile waits for Core Ready, a responding Scout, and REST inventory for
all targets. These checks do not establish successful tenant OS installation.
See [validation evidence](VALIDATION.md) for the exercised scope.

### Networking

The private bridge `br-nico-vm` uses `10.84.30.0/24`, with the host at
`10.84.30.1`. BMC endpoints use `.101` and `.102`; NICo reserves these fixed
addresses. The helper rejects overlapping IPv4 routes before creating the
network. The private inventory records matching MAC addresses, serials and
UUIDs for BMC and libvirt.

NICo DHCPv4 owns guest address allocation. A relay forwards DHCP to the cluster;
Kubernetes DNS resolves provisioning names. Service routes and narrow forwarding
rules preserve guest source addresses for Scout identity checks. An IPv4
masquerade rule on the host's default uplink provides Internet egress. No
upstream gateway change, DHCPv6 server, or IPv6 route is required. The profile
does not provide tenant network isolation.

## Persistent state and cleanup

State defaults to `~/.local/share/nico-vm-hosts`. Set `LOCAL_DEV_VM_HOSTS_DIR`
to a different absolute directory before initial deployment. One lab using these
resource names can run per host; resources owned by another directory are
rejected. Inventory records the architecture and resource sizes. Legacy
inventories from the IPv6 experiment are not adopted by this profile.

The global `--root` option precedes the subcommand. Direct `up` requires the
built `nico-devspace-libvirt:latest` and `nico-devspace-bmc-mock:latest` images:

```bash
python3 dev/deployment/devspace/vm-hosts/lab.py status
python3 dev/deployment/devspace/vm-hosts/lab.py stop
python3 dev/deployment/devspace/vm-hosts/lab.py up --count 2
```

`stop` saves running guests and removes containers, forwarding rules and service
routes. It retains guest disks, UEFI variables, domain definitions and BMC state.
`up` recreates containers and restores guests saved by `stop` in a paused state.
Setup resumes paused lab guests only after service routes and DHCP are ready.
Guests without a saved memory image remain off for NICo to power on. Re-run
the DevSpace profile after `stop` to reconcile the relay and site integration.

Direct `up` options are `--count` (default `2`, range 1–2), `--memory-mib`
(default `4096`, range 1024–65536), `--cpus` (default `2`, range 1–32) and
`--disk-gib` (default `24`, range 8–1024). Omitted resource sizes reuse the saved inventory after initial creation;
explicit size changes are rejected. To grow a one-target lab while preserving the first target:

```bash
python3 dev/deployment/devspace/vm-hosts/lab.py grow --count 2
export LOCAL_DEV_VM_HOST_COUNT=2
bash dev/deployment/devspace/vm-hosts/setup.sh
```

`grow` requires existing inventory, accepts `1` or `2`, rejects shrinking and
preserves existing resource sizes. Repeating the existing count is idempotent.

BMC snapshots preserve credentials, BIOS/boot-order overrides and manager
host-interface settings. Older account-only snapshots cannot recover settings
that were never persisted. Credentials remain in the private state directory,
not the checkout. Keep this directory with the site's database and Vault:
`devspace purge --profile vm-hosts` rejects retained inventory, and setup rejects
a cluster identity change.

After releasing instances through NICo, explicitly destroy a disposable lab:

```bash
python3 dev/deployment/devspace/vm-hosts/lab.py destroy --delete-disks
```

This permanently removes target disks and BMC state, not NICo's database records.
The `--delete-disks` flag is required; deployment never invokes destruction.

## Console and tests

Serial output is retained at `logs/<target-name>-console.log` in the state
directory. Connect interactively with:

```bash
docker exec -it nico-vm-hosts virsh console nico-vm-hosts-1
```

Press `Ctrl+]` to detach. Credentials follow the boot or tenant image's policy.

Portable tests require neither KVM nor IPv6:

```bash
python3 -m unittest discover -s dev/deployment/devspace/vm-hosts
bash dev/deployment/devspace/setup-rest-integration_test.sh
```

Full validation additionally checks real Scout ingestion, tenant imaging,
connectivity, reboot, release, and persistence across BMC recreation. Record
source/artifact revisions and collect console and controller logs without
credentials.
