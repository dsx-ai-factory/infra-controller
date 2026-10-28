# Native lab acceptance tests

These tools create test resources and exercise two installed instances. Stack
and BMC runtime deployment do not import or require them. Start with the
[fresh lab procedure](../validation.md); use the references below for inputs,
observed contracts, retained journals and recovery. Component regression tests
remain alongside the deployment code they protect.

## Allocate and recycle an instance

Use a fresh lab target that has completed the preceding Scout admission checks.
Run Core commands through `dev/deployment/core-admin.sh` with
`LOCAL_DEV_KUBE_CONTEXT` set to the lab's explicit kind context. The Core admin
path bypasses the REST database; use it only in this isolated lab. The
`--cloud-unsafe-op` argument is required for VPC and instance mutations.

The exercised image boundary is a native Ubuntu 24.04 cloud image with root
filesystem label `cloudimg-rootfs`, the installer default. Other images need
explicit filesystem identification (`--rootfs-label` or `--rootfs-id`) and
separate validation: without a matching root filesystem, the installer cannot
seed cloud-init or install the SSH key.

For normal runs, use the [catalog helper](#owned-os-and-vpc-catalog) and
[lifecycle runner](#repeat-the-instance-lifecycle). They register the verified
image and flat VPC, generate cloud-init, select admitted hosts and retain the
allocation/release journal. Flat instances use the host's `HostInband` segment;
they require no DPU or overlay. The whole `/dev/vda` disk is replaced during
installation. The imager fetches the OS during provisioning, so a successful
container build does not prove download or installation.

Use the following lower-level commands to diagnose a selected instance.

For a native Ubuntu 24.04 image using systemd-networkd, the shared generator
produces this OS configuration and its on-disk network profile:

```bash
python3 dev/deployment/lab/acceptance/guest-config.py \
  --os-image-id "$os_image_id" --mac 02:00:00:01:00:02 \
  --public-key /tmp/nico-instance-key.pub --generation generation-2 \
  --network ipv6-only > /tmp/nico-instance-os.json
```

The image UUID, guest MAC, public key file and nonempty printable generation marker are required.
The marker and public key line must contain only characters accepted by
Python's `str.isprintable()` (ASCII space is allowed); a whitespace-only marker
is rejected. The MAC must be a nonzero unicast Ethernet address. The key
file must contain one OpenSSH public key; `ssh-keygen` validates it. Private keys
are rejected. The command only prints JSON: it does not contact Core, create
files or allocate resources. Redirect its output to your chosen OS configuration
file and use that file with the allocation command below.

`--network` accepts `ipv4` (default), `dual-stack` and `ipv6-only`. The profiles
select DHCPv4, DHCPv4 plus DHCPv6, and DHCPv6 respectively. IPv6 profiles accept
router advertisements and explicitly solicit DHCPv6; they require the
[managed DHCPv6 target setup](../README.md#managed-dhcpv6-guests). IPv4 link-local fallback is
disabled in every profile. No static address or DNS server is supplied; DNS must
come from the lab's DHCP/RA configuration. No SLAAC addresses are requested by
this lab's router advertisements.

The generated cloud-init creates user `lab` with passwordless sudo and the
provided public key, writes `/etc/nico-lab-generation`, and installs
`/etc/systemd/network/05-nico-lab.network` matched to the guest MAC. That file
takes precedence over the image's generated networkd file. Cloud-init reloads
networkd after writing it; before that reload, Core's default NoCloud network
configuration enables both DHCPv4 and DHCPv6. This fixture targets the lab's
single guest NIC; it does not disable networking on additional interfaces. Subsequent disk boots use the persistent selected profile.
These profiles do not alter firmware, Scout or imager provisioning networking.
Other operating systems or network managers require their own configuration.

For `ipv6-only`, connect using Core's allocated IPv6 address, wait for cloud-init
to finish, and verify that `ip -j address` has no IPv4 address outside loopback
and that `ip -4 route show table all` has no non-loopback route. Repeat those
checks after Core reboot. Keep a separate known-hosts file for the new generation
and confirm that the old generation's disk marker is absent.

With `machine_id`, `vpc_id` and `os_config` set to the selected host ID, VPC ID
and JSON file path:

```bash
bash dev/deployment/core-admin.sh --cloud-unsafe-op 'Local instance validation' \
  instance allocate --prefix-name native-lifecycle \
  --machine-id "$machine_id" --flat-vpc-id "$vpc_id" --os "$(cat "$os_config")"
bash dev/deployment/core-admin.sh -f json instance show "$machine_id"
```

Save the created instance ID. Check the API response even if allocation exits
zero: the nontransactional CLI can log an allocation failure without a nonzero
exit status. API tenant `Ready` does not prove the OS has booted. Follow the
selected target's `/data/logs/console.log` through imager completion and the
installed OS boot before using the SSH helper. Scout and the installed OS have
different SSH host keys; do not enroll Scout's key as the tenant's key.

The [lifecycle runner](#repeat-the-instance-lifecycle) verifies the installed root,
cloud-init, generation marker, finite leases and peer connectivity. It flushes a
disk marker before reboot, requires a changed boot ID and preserved marker,
then releases to Ready/live Scout and repeats allocation with a new generation.
Use it to perform these checks reproducibly. For manual recovery, use the saved
instance ID and inspect Core and the selected target's console after each command:

```bash
bash dev/deployment/core-admin.sh --cloud-unsafe-op 'Local instance validation' \
  instance reboot --instance "$instance_id"
bash dev/deployment/core-admin.sh --cloud-unsafe-op 'Local instance validation' \
  instance release --instance "$instance_id"
bash dev/deployment/core-admin.sh -f json machine show "$machine_id"
bash dev/deployment/core-admin.sh scout-stream ping "$machine_id"
```

Keep Core, Vault and target storage together until release completes. A new
instance requires a separate known-hosts file; the old guest's key must not be
accepted for its replacement.

This proves installation and replacement of a virtual disk image, not hardware
secure erase. Scout's hardware cleanup discovers NVMe and `/dev/sd*` devices;
the target's `/dev/vda` is outside those paths. Do not infer tenant data erasure
at release from a successful reprovisioning test. Virtio inventory, hardware
sanitize coverage, SLAAC and Internet IPv6 remain separate acceptance boundaries in
[the migration issue](https://github.com/dsx-ai-factory/infra-controller/issues/8124).

## Two-instance IPv4 connectivity

Use two targets on the same node of the dedicated single-node kind cluster.
The target pods use `hostNetwork`: their separate bridges share that node's
network namespace. Connected routes and the existing provisioning forwarding
rules provide the peer path. Each target exempts packets destined for its own
subnet and exiting through its bridge from NAT before kindnet's catch-all
masquerade rule. This preserves guest source addresses on the peer path.
This topology is an isolated flat lab, not a DPU overlay or tenant-isolation test.

Create a separate PVC and values file for each target using the
[BMC target chart procedure](../../../../helm/nico-bmc-target/README.md).
Keep the same cluster UID, node, architecture, image and provisioning settings,
with these distinct values:

| Value | `target-one` | `target-two` |
| --- | --- | --- |
| `storage.existingClaim` | `target-one-data` | `target-two-data` |
| `target.instanceIndex` | `1` | `2` |
| `network.bridgeAddress` | `10.84.40.1/24` | `10.84.41.1/24` |
| `service.clusterIP` | `10.96.40.10` | `10.96.40.11` |
| `service.targetPort` | `2443` | `2444` |
| Host MAC | `02:00:00:01:00:02` | `02:00:00:02:00:02` |
| BMC MAC | `02:00:00:01:00:01` | `02:00:00:02:00:01` |
| Chassis serial | `020000010001` | `020000020001` |

The release, PVC and identity must remain distinct. Budget for both guests and
runtime overhead before deploying them. Register a HostInband segment for each
bridge subnet, using its bridge address as gateway and reserving the first ten
addresses. The existing `10.96.40.8/29` underlay segment contains both BMC Service
addresses. Register each expected machine with its own identity, BMC address and
metadata name. Admit both through real Scout, then allocate one disk-installed
instance per machine in the same flat VPC using the [lifecycle runner](#repeat-the-instance-lifecycle). Check the Core-assigned addresses; do not assume these examples were
allocated. Wait for both installed operating systems and cloud-init to finish.

The checker needs a Linux or macOS client with Python 3.9+, OpenSSH and kubectl,
Python 3 and
SSH access inside both guests, and guest firewall permission for TCP connections
to ephemeral ports. Use the SSH key installed in both guests' cloud-init data:

```bash
python3 dev/deployment/lab/acceptance/check-connectivity.py \
  --context kind-nico-tilt \
  --first-address 10.84.40.10 --second-address 10.84.41.10 \
  --identity /tmp/nico-instance-key \
  --first-known-hosts /tmp/nico-first-known-hosts \
  --second-known-hosts /tmp/nico-second-known-hosts
```

Required options have no defaults: `--context`, `--first-address`,
`--second-address` (both IPv4 or both IPv6), `--identity`, `--first-known-hosts` and
`--second-known-hosts`. Defaults are `--namespace bmc-lab`, `--user lab`,
`--first-release target-one` and `--second-release target-two`. Repeated options
use the last value. The addresses and release names must be distinct; the
context must start with `kind-`. Each connection uses the
[SSH helper's validation and host-key rules](../README.md#ssh-to-an-installed-guest).
Use separate host-key files for newly provisioned instances.

The check runs both directions sequentially. In each direction the receiver
binds its guest address and an OS-selected TCP port, while the sender binds its
own guest address. The receiver verifies the peer source address and a random
32-byte payload, then echoes the payload for the sender to verify. SSH only
starts these programs; the test payload travels directly between the guests.
No guest files, persistent services, firewall rules or routes are changed.
The receiver accepts one connection, waits at most 30 seconds for it, and uses
10-second socket timeouts for data. It expires if the client is interrupted.
Client-side waits for listener startup and the sender are each bounded to 30
seconds, followed by at most 15 seconds for the receiver's result. Local SSH
and relay process groups are stopped on completion, Python exceptions or Ctrl-C.
SIGTERM, SIGHUP and forced termination do not guarantee local process cleanup.

Success prints one JSON object with `directions`, containing the verified
`source`, `destination` and payload `bytes` for each direction, and exits 0.
A failed probe prints an error to stderr and exits 1 without a success object;
argument parsing errors, non-kind contexts, mixed address families and duplicate addresses/releases exit 2.
Validation failures inside the SSH helper (such as a missing key, invalid path
or address outside the target subnet) instead fail the probe with exit 1.
`--help` prints usage and exits 0. A failed second
direction does not erase the fact that the first connection was attempted.
The check neither allocates nor releases instances and leaves their disks intact.

The [lifecycle runner](#repeat-the-instance-lifecycle) repeats these checks across
reboot and release while verifying the surviving peer. DPU routing and secure
erase remain separate validation boundaries.

## Two-instance IPv6 routing

Use the [managed DHCPv6 guest setup](../README.md#managed-dhcpv6-guests)
and the lifecycle runner with `--network dual-stack` or `--network ipv6-only`.
The runner reads Core's allocated addresses and checks both peer directions
before and after reboot. It uses the same direct-socket checker described above;
static addresses and hand-written per-peer routes are unnecessary.

This is a flat no-DPU lab. It does not establish DPU isolation or SLAAC support.
IPv6 loopback regression tests skip when unavailable and do not count as native
two-guest acceptance. Internet IPv6 is opt-in through `--internet-ipv6`.

## Repeat the instance lifecycle

`lifecycle.py` operates on two explicitly selected, already admitted no-DPU lab
machines. Deploy the stack and targets, register the native OS image with its
SHA-256 digest, and create a flat VPC with the [catalog helper](#owned-os-and-vpc-catalog) first.
Before allocating, the runner waits for both targets to be Ready with live Scout
streams, using the same per-target `--timeout` as later lifecycle waits. This
allows Scout to reconnect after recovery; a recorded Ready state alone is not
sufficient. No allocation starts if either initial wait fails. The OS image must
support the Ubuntu/networkd cloud-init fixture; the root disk must be `/dev/vda`.
Choose the same `--network` profile used to register these targets with
`register.py`. The bridge can expose more address families than Core allocates;
a missing requested family is detected after allocation and requires
`--release-only` cleanup. Extra allocated families are ignored by the runner and
left unconfigured in the guest; an IPv6-only guest result alone does not prove
that Core allocated only IPv6. For IPv6 guests, deploy the managed DHCPv6 relay, resolver and routes first.
The DHCP runtime must include the
[boot-stage lease handoff fix](https://github.com/dsx-ai-factory/infra-controller/pull/7934).

Select a new private directory whose parent exists. Replace the example Core IDs
with those returned by your lab's Core API:

```bash
python3 dev/deployment/lab/acceptance/lifecycle.py \
  --context kind-nico-tilt --state-dir "$HOME/nico-lifecycle-run" \
  --target "target-one=$FIRST_MACHINE_ID" \
  --target "target-two=$SECOND_MACHINE_ID" \
  --vpc "$VPC_ID" --os-image "$OS_IMAGE_ID" --network ipv6-only
```

The context, two target pairs, VPC UUID, OS-image UUID and state directory are
required for a new run. Releases and machine IDs must both be distinct. Only
explicit `kind-` contexts and deployments labeled `nico-bmc-target` are accepted.
The runner verifies cluster UID, inventory owner and Core BMC address/MAC before
allocating; an existing state directory is rejected. Directory paths cannot
contain whitespace, quotes, `%`, `$` or backslashes because SSH pins keys there.

`--namespace` defaults to `bmc-lab`; both targets use it. `--core-namespace`
defaults to `nico-system`. `--network` accepts `ipv4` (default), `dual-stack` or
`ipv6-only`. `--cycles` accepts 1 or 2 and defaults to 2. `--timeout` is the
per-transition polling deadline in seconds, defaults to 1800 and accepts 60–7200;
individual command deadlines can extend a final poll beyond that boundary.
There is no total run deadline. `--internet-ipv6` is false by default and requires
an IPv6 guest profile. It adds certificate-verified IPv6 HTTPS to
`https://www.google.com` for both guests before and after each reboot. Omission
means external IPv6 was not tested; internal IPv6 remains tested for IPv6
profiles. It is suitable for an environment without an external IPv6 route.

Each cycle allocates two distinct instances, verifies the current disk generation,
cloud-init and finite DHCP leases, and checks bidirectional TCP with unchanged
peer source addresses. IPv6-only guests must have no non-loopback IPv4 address or
route and must fail an IPv4 Internet ping. Each guest is then rebooted through
Core: its disk marker must persist and its boot ID must change while its peer's
boot ID stays unchanged. Connectivity is checked again. The runner releases the
first guest and checks that its peer remains reachable with the same boot ID,
then releases the second, waiting for Ready
and live Scout after each release. A second cycle repeats allocation from these
released machines. This does not rebuild the stack or prove a fresh-cluster run.

Readiness verifies the disk root, cloud-init completion and allocation generation
over SSH using provisional host keys. Dual-stack runs verify the same boot ID
through both addresses before publishing either key. Only a successful verification
persists the generation's keys; failed probes cannot pin an imager or previous guest.
Serial-log rotation does not affect readiness. Existing pins remain authoritative.

The directory contains a newly generated SSH key, per-generation pinned host
keys, allocated identities, network observations and an atomic `state.json`
journal. Treat it as private; do not commit or share it. Only one process may use
a journal at a time. Allocation intent is flushed to disk, including the parent
directory entry, before the API call, so a lost
response can be reconciled without adopting an unrelated instance. A failure
retains the journal and allocation output, and collects target consoles and
machine state in a new diagnostic directory without overwriting prior evidence.
Diagnostic collection skips targets whose ownership cannot be verified; it never
force-clears Core state or silently retries allocation.

After a failed run, release only its recorded instances with:

```bash
python3 dev/deployment/lab/acceptance/lifecycle.py \
  --state-dir "$HOME/nico-lifecycle-run" --release-only
```

Cleanup reads context and identities from the journal, revalidates target
ownership, and rejects replaced clusters, targets or instances. It attempts each
target independently, reports all failures, and returns nonzero if any remain. Do not supply
`--context`, `--target`, `--vpc` or `--os-image` with `--release-only`. Other run
settings are read from the journal. Repeating cleanup after successful release
is harmless. Keep journals in a persistent directory: a reboot may clear `/tmp`.
Run directories support cleanup, not resuming an interrupted test;
start a new directory for another run. Neither mode deletes VPCs, OS catalog
entries, target PVCs, the stack or its credentials. Exit status is zero only when
all selected checks or cleanup complete; failures return nonzero and retain
evidence.

## Continuous IPv6 traffic guard

Wrap the two-target lifecycle command so observation starts before allocation
and ends after release and Scout cleanup. This requires the
[Tilt IPv6 boot profile](../../tilt/README.md), with `--ipv6-boot`,
`--ipv6-endpoints`, and IPv4 provisioning disabled on both targets. A default
IPv4 Scout/imager boot is expected to fail this guard:

```bash
python3 dev/deployment/lab/acceptance/traffic-guard.py \
  --context kind-nico-lab --target target-one --target target-two \
  --output "$HOME/nico-ipv6-traffic" -- \
  python3 dev/deployment/lab/acceptance/lifecycle.py \
    --context kind-nico-lab --state-dir "$HOME/nico-ipv6-lifecycle" \
    --target "target-one=$FIRST_MACHINE" --target "target-two=$SECOND_MACHINE" \
    --vpc "$VPC_ID" --os-image "$OS_IMAGE_ID" --network ipv6-only --cycles 2
```

Use the selected lab's actual context, releases and IDs. The wrapper requires
Python 3.9+, kubectl, an accessible kind cluster and two running native BMC target
pods with Python and Linux raw-packet and receive-buffer access (`CAP_NET_RAW` and `CAP_NET_ADMIN`). It verifies the
cluster UID, target ownership, distinct guest MACs and bridges, and captures
inside each selected pod. The command runs locally only after both captures are
ready. `--namespace` defaults to `bmc-lab`; both releases must use it.
`--output` is a required new directory under an existing parent, created with
private permissions. `--timeout` is the wrapped command's positive duration in
seconds, 60–86400 inclusive, default 14400. Put all wrapper options before `--`;
the remaining arguments form the command, without shell expansion by the wrapper.
The command receives no stdin. Cancellation is recorded without interrupting
evidence cleanup; repeated signals do not shorten the cleanup grace period.
During startup, cancellation waits for the bounded setup of both captures.
While the command runs, cancellation is checked at least once per second.
Cancellation during evidence collection also makes the run fail. The completion
boundary is the recorded finish time, after capture and identity checks; signals
received while writing the final report do not change that completed result.
Timeout or interruption sends SIGINT to the command's process group, allowing the lifecycle runner to stop its own children and collect
diagnostics. Captures continue during a grace period of up to 300 seconds before
a forced SIGKILL. Commands must handle SIGINT and clean up children they start
in separate sessions; a forced stop is recorded as incomplete recovery.

Each target's `.jsonl` file contains readiness, periodic cumulative counters and
an explicit completion record. `.stderr` retains observer errors; `report.json`
records the command arguments, exit code or error, cancellation signal, UTC start/end times, forced
termination status, selected targets and guard result. Use the command record to
distinguish a smoke run from a full lifecycle; the wrapper does not infer or
validate the wrapped command's target selection. No packet payloads are stored.
All IPv4 and ARP frames on each dedicated guest bridge count, including
broadcast DHCP replies. IPv6 and other Ethernet traffic count only when the guest
MAC is a source or destination. Capture uses temporary promiscuous socket
membership and a 4 MiB receive buffer; closing the socket releases both. IPv6 must be
observed on both targets; ARP, other IPv4 traffic, malformed IP frames, kernel
capture drops, early observer exit, or target/cluster replacement fail the guard.
Other Ethernet protocols are counted separately. An empty capture cannot pass.

Firmware may broadcast DHCPv4 client probes before trying IPv6. Unfragmented
UDP 68-to-67 frames sent by the guest from `0.0.0.0` to `255.255.255.255` are
reported as `dhcp4_probe`, not successful IPv4 use. Replies and assigned-source
IPv4 are violations. This exception describes the transport tuple; the observer
does not decode DHCP message options. The guest's IPv4 address/route checks and
negative control remain part of the lifecycle runner.

Captures drain queued frames at the stop boundary; inability to drain produces
incomplete evidence. Failure retains all logs and does not release instances or
remove infrastructure. Use the lifecycle journal's `--release-only` procedure
for recovery. A short observer smoke run proves capture wiring only: full-window
evidence requires wrapping the entire lifecycle command. Guest-bridge evidence
does not establish IPv6-only BMC or control-plane transport, DPU forwarding, or
Internet reachability; those require their separate checks.

## Owned OS and VPC catalog

After loading and serving the OS bundle, register its image and a separate flat
VPC through the existing Core admin client:

```bash
python3 dev/deployment/lab/acceptance/catalog.py \
  --context kind-nico-lab --state-dir "$HOME/nico-lab-catalog" \
  --artifact-manifest /tmp/nico-os-validation/build-info.json \
  --pxe-origin http://carbide-pxe.forge:8080
python3 dev/deployment/lab/acceptance/catalog.py --state-dir "$HOME/nico-lab-catalog" --resume
```

The IPv6 boot profile uses the origin above. For the default boot profile, use
`http://nico-pxe.nico-system.svc.cluster.local` for the default Core namespace;
substitute the selected namespace when using `--core-namespace`. The origin must be reachable by
the guests; this helper does not upload or serve the image or test its download.
The manifest comes from `build-os-artifact.py`; its SHA256 and native PXE path
become the catalog entry. The kind cluster must contain one node matching the
manifest's architecture. Core and its admin-client credentials must already be
ready; [the Core client wrapper](../../core-admin.sh) supplies that connection.

Python 3.9+, Bash, jq and kubectl are required. For a new catalog, `--context` must name a
kind context; `--artifact-manifest`, `--pxe-origin` and `--state-dir` are required.
`--core-namespace` optionally selects the Core namespace, default `nico-system`.
`--namespace` remains a compatibility alias.
`--pxe-origin` accepts HTTP or HTTPS with an optional port and trailing slash,
without any other path, credentials, query or fragment. `--state-dir` must be a
new directory beneath an existing parent; relative paths use the current working
directory. It is created privately and locked against concurrent operations.
`--resume` uses the saved configuration and rejects the four creation options.

The helper creates a generated tenant-organization identifier, one Ready OS
catalog entry targeting `/dev/vda`, and one flat VPC with matching ownership
metadata. It prints JSON containing `context`, `cluster`, `owner`, `os_id` and
`vpc_id`; supply the latter two IDs to the lifecycle runner. No instance, target,
network segment or REST tenant is created. Flat networking still relies on the
registered HostInband segments and host routes.

Creation intents are atomically saved and synchronized before their API calls.
`state.json` retains cluster identity, owner, resource IDs, URL and artifact
provenance; separate creation logs retain CLI output. A resume verifies ownership,
Ready image status and relevant configuration before returning existing IDs. A
changed cluster, ambiguous VPC match, replaced ID or changed image/networking
fails without overwriting the remote resource.

Core generates VPC IDs and does not treat the name as an idempotency key. After
an interrupted create, resume adopts only a uniquely matching visible result.
If none is visible, it stops with an unknown-outcome diagnostic and never repeats
the create. Inspect the retained log and Core, then resume after the result is
visible. If creation is confirmed absent, this journal remains stopped: start a
new catalog directory, preferably after recreating the disposable cluster. Any
resources already created by the old journal remain in that cluster until its
explicit removal. A deliberately removed resource is not silently recreated.
Keep the old journal as evidence; rerunning with a new directory in the same
cluster creates a separate catalog.

The catalog and VPC live for the lifetime of this dedicated disposable cluster.
The helper provides no remote deletion operation: release all owned instances
through the lifecycle runner, then explicitly remove the dedicated cluster when
its evidence has been collected. Retained journals cannot be reused with a
replacement cluster; create a new catalog there. No database editing is involved.

## Register owned target networks and machines

After deploying two native BMC targets in a dedicated kind lab, register their
provisioning networks and expected-machine identities through Core:

```bash
python3 dev/deployment/lab/acceptance/register.py \
  --context kind-nico-lab --state-dir "$HOME/nico-target-registration" \
  --target target-one --target target-two --network ipv6-only \
  --bmc-prefix fd00:10:96::4010/124 \
  --bmc-password-file /tmp/nico-lab-initial-bmc-password
python3 dev/deployment/lab/acceptance/register.py \
  --state-dir "$HOME/nico-target-registration" --resume
```

Use the actual target release names and a dedicated prefix containing their BMC
Service addresses. The password file must already contain their initial BMC
password as one nonempty line, be owned by the invoking user, and have no group
or other permissions. Keep it private. The helper does not generate or replace
BMC credentials. Its path is recorded; its contents are not saved in the journal.
During first registration, the initial credential appears in the process
arguments of the host wrapper, kubectl and the admin CLI. Use a trusted lab host. Resume does not need that file after both registrations exist.
Expected-machine creation logs deliberately retain only completion or failure
status: CLI output and exception text can include the credential. Diagnose a
failed create through Core service logs and the read-only `em show` command;
the local creation log does not retain its rejection reason. Segment creation
logs retain normal CLI diagnostics.

Python 3.9+, Bash, jq, kubectl and the configured Core client wrapper are required.
For a new journal, `--context`, two distinct `--target` values, `--bmc-prefix`,
`--bmc-password-file` and `--state-dir` are required. The context must start with
`kind-`; both targets must be native BMC deployments belonging to that cluster.
Their architecture must match its single node. `--core-namespace` selects Core
(default `nico-system`); `--namespace` selects both targets (default `bmc-lab`). `--bmc-username` defaults to `root`. `--domain` defaults to the existing
`local.nico` DNS domain and must resolve to exactly one live Core domain.

`--network` selects the guest prefixes to register: `ipv4` (default), `dual-stack`
or `ipv6-only`. Prefixes come from each target's saved bridge configuration.
Each gateway must be among its subnet's first nine addresses, which remain
reserved by `--reserve-first 10`. IPv4 segments have their bridge gateway;
IPv6 routing comes from the target's managed advertisements. At most one
`--bmc-prefix` is accepted per address family. Each BMC must be inside its
corresponding prefix, excluding both endpoints. All selected prefixes and target
identities must be distinct; overlaps with existing external segments fail.
This helper does not configure Services, bridges, DHCP relays, RA, DNS resolvers
or routes. Configure those through the target chart and stack before registration.

The helper saves generated resource IDs and creation intents in a new private
state directory, verifies target ownership and checks existing Core resources
before writing. It creates the BMC underlay and each HostInband segment, waits for
Ready, then creates expected machines with DPU policy `ignore` and DPF disabled.
`--timeout` bounds segment readiness polling in seconds (default 300, range
60–1800); individual API calls can extend the final poll. Registration starts
normal Core discovery and provisioning. Success prints release and expected-ID
pairs, not a claim that machines are Ready. Obtain machine IDs with
`machine show`, then use the lifecycle runner, which waits for Ready and live Scout.

`--resume` accepts only the existing `--state-dir` and rechecks the saved cluster,
target inventories and resource configuration. It never updates expected
machines, resets rotated credentials or repeats an uncertain create. If a
recorded create is absent, retain the journal and inspect Core; a later visible
matching resource can be adopted. A confirmed-absent result requires a new
dedicated lab and registration journal. A new journal alone cannot adopt the old
segments or target identities; partially created resources remain until that lab
is explicitly discarded. Changed, conflicting
or replaced resources fail without adoption. The directory is locked against
concurrent invocation. Neither mode deletes resources or target disks.
