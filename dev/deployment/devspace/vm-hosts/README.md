# DevSpace with real zero-DPU target VMs

The opt-in `vm-hosts` profile inherits `full` and replaces machine-a-tron with
native KVM guests. A standalone `bmc-mock` controls each guest through libvirt.
The default `full` profile continues to use machine-a-tron.

This profile exercises discovery and OS provisioning on real guest operating
systems. This extension layers dual-stack networking on
[PR 7564](https://github.com/dsx-ai-factory/infra-controller/pull/7564) for
[issue 84](https://github.com/dsx-ai-factory/infra-controller/issues/84).
DHCPv6 lease handoff fixes belong to
[PR 7934](https://github.com/dsx-ai-factory/infra-controller/pull/7934); include
that fix when validating changes in client identity between firmware and Linux.
The generic BMC behavior belongs to
[PR 7948](https://github.com/dsx-ai-factory/infra-controller/pull/7948).
The vfkit environment and MAT tests
remain in [PR 6606](https://github.com/dsx-ai-factory/infra-controller/pull/6606),
[PR 7430](https://github.com/dsx-ai-factory/infra-controller/pull/7430), and
[PR 7436](https://github.com/dsx-ai-factory/infra-controller/pull/7436).
It does not emulate BlueField forwarding, DPU agents, GPU devices, or hardware
isolation.

## Requirements

Use an x86_64 or ARM64 Linux Docker host with `/dev/kvm`, Python 3.11 or later,
PyYAML, the normal DevSpace prerequisites, and a single-node kind cluster whose
provisioning services have matching node address families, including an IPv6
`nico-dhcp6` service and IPv6 kind node address. A physical host needs hardware virtualization;
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
See [backend validation evidence](VALIDATION.md) and the
[IPv6 validation record](IPV6-VALIDATION.md) for the exercised revisions and scope.
The validation record distinguishes historical runs from the later combined
PR integration and its runtime configuration.

### Networking

The private bridge `br-nico-vm` uses `10.84.30.0/24` and
`fd42:84:30::/64`, with the host at `.1` and `::1`. BMC endpoints use
`10.84.30.101` and `10.84.30.102`; NICo reserves these fixed addresses.
The helper rejects overlapping IPv4 or IPv6 routes before creating the network.
The private inventory records matching MAC addresses, serials and UUIDs for
BMC and libvirt.

NICo DHCP owns guest address allocation. The DHCPv6 relay adds RFC 6939 client
link-layer metadata from the actual Ethernet frame, including when UEFI uses a
UUID-based DUID. It accepts only configured target MACs on the flat, untagged
link; VLANs, IPv6 extension headers/fragments, and nested relays are unsupported.
Router advertisements supply the on-link prefix and default route, with
stateful DHCPv6 addressing rather than SLAAC.

Kubernetes DNS resolves provisioning names. Service routes and forwarding rules
preserve guest source addresses for Scout identity checks. The base profile's
IPv4 masquerade rule provides IPv4 Internet egress. IPv6 egress requires the
upstream lab gateway to route `fd42:84:30::/64` back through the host and permit
it in its egress/NAT rules. The helper does not modify that gateway or install
NAT66 on the host. Successful DHCP or peer traffic alone does not establish
Internet IPv6 reachability. The profile does not provide tenant network isolation.

Use a fresh lab network and site for this dual-stack extension: it does not
upgrade an existing IPv4-only Docker network or HostInband segment in place.

### IPv6-only host-facing provisioning

After the `vm-hosts` profile has configured the lab, run this on its Linux
Docker host in the same kind context:

```bash
python3 dev/deployment/devspace/vm-hosts/configure_ipv6.py --help
python3 dev/deployment/devspace/vm-hosts/configure_ipv6.py \
  --output "$HOME/.local/share/nico-vm-hosts/ipv6-provisioning"
```

`--output` is required and holds original configuration snapshots, generated
Service manifests, and `ipv6-services.json` with the allocated addresses.
Reruns preserve the original snapshots. The helper uses
`LOCAL_DEV_VM_HOSTS_DIR` and `LOCAL_DEV_NAMESPACE` with the same defaults as
setup. It requires the existing inventory, cluster identity and network state,
a single-node kind context, and an IPv6 kind node address. It rejects a
snapshot directory from another cluster, unrelated companion Service names,
conflicting routes, ambiguous DNS blocks, and ambiguous NICo DHCPv6 hooks.
The host needs `kubectl`, `kind`, Docker, `ip`, and permission to run `sudo ip`.

The helper adds IPv6-only companion Services for Core, PXE and cluster DNS,
retaining their original Services and ports. It routes their `/128` addresses
through the kind node, adds provisioning AAAA records alongside existing A
records, and configures the DHCPv6 hook's DNS and provisioning-server addresses.
A pod-template configuration digest reconciles CoreDNS and DHCPv6 even when a
previous run stopped after updating the ConfigMap. Newly created routes use
the existing lab cleanup ledger; matching pre-existing routes are left alone.

This is a separate lab preparation step, not an additional DevSpace profile.
Base setup can regenerate the managed DNS block, so run this helper after base
setup when using the IPv6-only experiment. Host routes need re-establishing
after the outer VM restarts. Companion Services and ConfigMaps remain in the
cluster after `lab.py stop`; this helper does not restore snapshots automatically.

The remaining lifecycle steps require explicit orchestration: serve and
register an OS image reachable through IPv6, verify IPv6 Redfish access before
changing each BMC registration, and supply tenant cloud-init network
configuration with `dhcp4: false`, `dhcp6: true`, `accept-ra: true`,
`link-local: [ipv6]`, and IPv6 DNS. Copy an authorized public SSH key into the
tenant configuration. Keep guest disks, NICo/Vault state and BMC credentials
paired. The helper does not allocate, release, reboot or modify guests or BMCs,
and does not configure the upstream gateway or build images.

Mixed address/prefix DHCPv6 requests are handled by
[PR 7702](https://github.com/dsx-ai-factory/infra-controller/pull/7702).
Do not infer a missing product fix from the historical experiment's
`UseDelegatedPrefix=no` override. Include
[PR 7728](https://github.com/dsx-ai-factory/infra-controller/pull/7728) to avoid
retaining the extracted network block in cloud-init user-data.

Copy `validate_ipv6_only.py` into each installed guest. Run it in both
directions, substituting the guest address, the other guest's address, the DNS
address printed by setup, and the interface name. For the recorded lab:

```bash
python3 /tmp/validate_ipv6_only.py --help
python3 /tmp/validate_ipv6_only.py \
  --address fd42:84:30::a --peer fd42:84:30::b \
  --dns fd00:10:96::fdbe --interface enp1s0
```

All four options are required; address options accept unicast, non-loopback
IPv6 addresses. The peer must not be any address configured on the local guest.
The validator requires Python 3, `ip`, `resolvectl`, `curl`, `ping`, cloud-init,
and noninteractive `sudo` for flushing the DNS cache and checking cloud-init's
schema. It does not change network configuration. It emits a JSON array and
exits zero only when every check passes; check failures exit one, invalid
arguments exit two, and missing tools or command timeouts also exit nonzero.

Checks cover IPv6 addressing, absence of non-loopback IPv4 addresses/routes,
IPv6-only DNS with the expected resolver, uncached AAAA resolution,
certificate-verified Google HTTPS with explicit IPv6 and normal address
selection, an IPv4 negative control, peer ICMP and SSH banners, and cloud-init
status/schema. Cloud-init warnings are failures, including the known warning
before PR 7728; review the exact diagnostic separately rather than treating a
nonzero result as a pass. The validator does not prove the root disk, instance
identity, BMC transport, boot-ID change, or provisioning transport. Those
require separate lifecycle evidence and an IPv4/ARP guard if proving that
provisioning cannot fall back to IPv4.

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
