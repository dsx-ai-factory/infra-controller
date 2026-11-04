# bmc-mock

`bmc-mock` is a standalone HTTPS Redfish simulator. Its optional libvirt mode
maps one `bmc-mock` process or container to one libvirt domain, matching the
real-world relationship between a Redfish endpoint and its host.

## Redfish response models

`build.rs` uses `nv-redfish-csdl-compiler` and the CSDL files bundled in
`nv-redfish-schema` to generate the response models selected in its root list.
Cargo writes them to `OUT_DIR/redfish_server.rs`; `redfish::schema` includes them.
The generated models use `nv-redfish-server-core` for references, finite numbers,
UUIDs, and OEM extension maps. Schema files are packaged dependencies, so generation
does not download schemas at build time.

Resource modules construct these models directly. Their required properties and
property types are checked by Rust, and entity `@odata.type` follows the resolved
schema. The root list also selects NVIDIA, Delta, Dell, HPE, and Supermicro OEM
models from the bundled vendor CSDL. OEM complex models use the `oem::object`
helper for the firmware's type discriminator, which the server generator emits
only for entities. Captured firmware spellings and older advertised type versions
remain explicit compatibility details.

Add new modeled resource types to the root list in `build.rs`, rather than writing
JSON builders. Runtime state, routing, capabilities, and fault injection remain in
`bmc-mock`. Explicit JSON is retained for OEM types absent from the bundled schemas
(`DelliDRACCard`, `DellOemResources`, and `DellNIC`), firmware deviations,
registry-defined BIOS/Dell attributes, bound actions, and expanded navigation
payloads that the server generator does not yet model.

## Build the libvirt image

Run from the infra-controller repository root:

```console
docker buildx build \
  --platform linux/amd64 \
  -f crates/bmc-mock/Dockerfile \
  --build-arg VERSION=0.1.0 \
  -t bmc-mock:latest \
  --load \
  .
```

Use `--push` instead of `--load` when publishing directly to a registry.
Omit `--platform` to build an image matching the local Docker engine's native
architecture. The Dockerfile supports both `linux/amd64` and `linux/arm64`
without running the Rust compiler under emulation.

## Run one endpoint per virtual machine

The container needs access to the libvirt daemon that owns the domain. On a
Linux host, mount the daemon socket and configure the domain name:

```console
docker run --rm \
  --name bmc-node-01 \
  -p 1266:1266 \
  -v /run/libvirt/libvirt-sock:/run/libvirt/libvirt-sock \
  bmc-mock:latest \
  --machine-role host \
  --state-backend libvirt \
  --libvirt-domain dsx-node-01 \
  --hardware-profile generic_ami \
  --libvirt-uri qemu:///system
```

The simulator never infers a hardware identity from a VM. Explicitly configured
endpoints select a role, state backend, and hardware profile. The older
`--libvirt-domain DOMAIN --hardware-profile PROFILE` form remains accepted as
shorthand for `--machine-role host --state-backend libvirt`.

| Hardware profile | DPU count |
| --- | ---: |
| `dell-poweredge-r750` | Variable; defaults to 0 |
| `dell-poweredge-r760-bf4` | 1 |
| `wiwynn-gb200-nvl` | 2 |
| `lenovo-gb300-nvl` | 1 |
| `nvidia-dgx-gb300` | 1 |
| `supermicro-gb300-nvl` | 1 |
| `nvidia-dgx-vr` | 1 |
| `nvidia-dgx-h100` | 1 |
| `generic-ami` | Variable; defaults to 0 |
| `generic-supermicro` | Variable; defaults to 0 |
| `hpe-proliant-dl380a-gen11` | Variable; defaults to 0 |

Use `--dpu-count` to populate a variable-count host profile. For a fixed-count
profile, the flag is optional and is validated if supplied.

Multiple containers on a bridge network can all listen on their internal port
1266 and use their container or service names as distinct BMC endpoints. Only
published host ports need to be unique.

Clients must discover the ComputerSystem instead of assuming a system ID:

```console
curl --insecure https://localhost:1266/redfish/v1/Systems
```

Do not assume the first member is the host. Some profiles expose both an HGX
baseboard and a controllable host. Fetch the members and select the resource
that contains `PowerState` for power, boot override, BIOS, and VirtualMedia
requests.

The default libvirt device targets are `sdb` for `Cd` and `sdc` for `ConfigCd`.
At startup, the backend creates an empty SATA CD-ROM at each configured target,
marked with a `ua-bmc-mock-vmedia-<device-id>` alias. An existing target must be
an owned SATA CD-ROM; a foreign device causes startup or media updates to fail.
Insert, replace, and eject use `update-device` to change media without removing
the CD-ROM. Updates affect saved configuration and, while the VM is active,
live configuration as well. Eject leaves an empty drive.

HTTP or HTTPS ISO URLs must be reachable from the host running the QEMU process. File paths must exist in that host's filesystem.

## Apply boot-order changes with libvirt

Persistent Redfish boot-order updates change the saved libvirt domain
configuration. For a running VM, request `PowerCycle` to apply a changed order,
or shut it down and then request `On`. Starting a stopped VM with `On` or
`ForceOn` also applies the effective boot selection.

`GracefulRestart` and `ForceRestart` retain their normal libvirt reboot and
reset behavior: they keep the running VM's boot configuration. They do not
activate a newly saved boot order. Temporary boot-source overrides likewise
need a cold start to take effect; after a one-shot override, the backend
restores the persistent selection in the saved configuration for a subsequent
cold start.

Libvirt publishes power-on and boot-completed lifecycle observations after a
successful start, reset or reboot is confirmed online, or after a real observed
Off-to-On transition. The first running observation and repeated running polls
only update power state. Boot completion is inferred from domain power.
Warm resets preserve a one-shot override staged for the next cold start.
Power commands wait up to 40 seconds for the first recognized observation before
validation. Expiry returns HTTP 500 without enqueueing a power command. Polling
continues, so a later request can succeed after an observation recovers. Ready
commands retain their existing enqueue acknowledgment. Execution failures are
logged and do not synthesize a completed BFB install.

## Run a separate DPU endpoint

A DPU is independently addressable hardware, so expose it through its own
`bmc-mock` process rather than adding its ComputerSystem to the host BMC
endpoint. This example exposes the second DPU from a Wiwynn GB200 host and uses
the in-process state machine because no DPU VM exists:

```console
bmc-mock \
  --port 8102 \
  --machine-role dpu \
  --state-backend internal \
  --hardware-profile wiwynn_gb200_nvl \
  --dpu-index 1 \
  --instance-index 1
```

Start the host and all of its DPU endpoints with the same `--hardware-profile`,
`--dpu-count`, and `--instance-index`. Their generated DPU serial numbers and
MAC addresses will then agree. `--state-backend libvirt --libvirt-domain NAME`
can be used for a DPU endpoint when a separate DPU VM exists.

The internal backend maintains power state entirely within the process. It does
not affect the host VM. A DPU using libvirt exposes only `ConfigCd` for
configuration media; a host using libvirt exposes `Cd` and `ConfigCd`. Internal
simulation keeps the profile defaults.

## SSE events

Start a generated Dell R750 mock with its hardware-configured EventService:

```bash
cargo run -p bmc-mock -- --machine-role host --state-backend internal \
  --hardware-profile dell_poweredge_r750 --port 1266
```

All generated hardware profiles configure EventService by default, including
hosts, DPUs, switches, and power shelves. A hardware profile can explicitly
return `None` to omit the service; no current profile does so. Archive-backed
streams remain unsupported. There is no separate SSE CLI flag.
Authentication remains disabled unless `--redfish-auth` is also
supplied. `--redfish-auth` applies only to generated routers and conflicts with
the archive-backed `--targz` and `--ip-router` modes. With authentication
enabled, rotate the profile's factory password through AccountService and
supply Basic credentials or a session token to both SSE and control requests.

In another terminal, discover and subscribe (the stream stays open):

```bash
curl --insecure https://localhost:1266/redfish/v1/EventService
curl --insecure --no-buffer https://localhost:1266/redfish/v1/EventService/SSE
```

Publish a test event from a third terminal:

```bash
curl --insecure https://localhost:1266/Mock/EventService/events \
  --header 'Content-Type: application/json' --data-binary @- <<'JSON'
{
  "@odata.id": "/redfish/v1/EventService/SSE#/Event1",
  "@odata.type": "#Event.v1_6_0.Event",
  "Id": "1",
  "Name": "Test event",
  "Events": [{
    "@odata.id": "/redfish/v1/EventService/SSE#/Events/1",
    "MemberId": "1",
    "EventType": "Alert",
    "MessageId": "ResourceEvent.1.2.ResourceRemoved",
    "Message": "Resource removed",
    "EventTimestamp": "2026-09-10T12:00:00Z"
  }]
}
JSON
curl --insecure https://localhost:1266/Mock/EventService/stats
curl --insecure --request POST https://localhost:1266/Mock/EventService/close
```

Mock-only controls live under `/Mock/<Service>/…`, beside the Redfish tree they
manipulate; new mock controls should follow the same prefix. They accept the
same authentication as the Redfish routes.

The mock also publishes its own lifecycle events, so an SSE consumer sees the
same traffic a real BMC produces: an accepted `ComputerSystem.Reset`, the
embedder's power-on and boot-completed notifications, and an IPMI chassis
reset. Each event carries a DMTF `ResourceEvent` message identifier, a
severity, and an `OriginOfCondition`. A `Manager.Reset` or the `/ipmi` mock
action `bmc_cold_reset` is logged but not announced, because the reset closes
every stream first. Profiles with a system `LogService`
(Dell R750, BlueField-3, BlueField-4) also append a matching `LogEntry` and
point the origin at it; other profiles point at the system resource.

A standalone reset is instantaneous unless `--bmc-reset-duration SECONDS` is
given: with it the mock answers 503 to every request for that long after
`Manager.Reset` or the `/ipmi` mock action `bmc_cold_reset`, then recovers on
the next request, as
embedded deployments already do from their platform timings. Either way the
reset closes streams, clears replay history, and starts a new generation.

Publication returns the opaque transport ID. Closing ends subscriptions while
preserving bounded replay history. A stalled reader retains its admission slot
until its response body is dropped; `/Mock/EventService/stats` reports these
bodies as `streams`, separately from active `subscribers`. CombinedServer closes
the connection after 60 seconds (the profile's `output_stall_timeout`) in which
either an emitted frame is never polled for again by the HTTP layer, or a
transport write stays blocked without any later write or flush completing.
Other HTTP/2 streams sharing that connection also end. Neither clock sees below the kernel: a reader
that stops consuming is detected once the socket send buffer fills, so with
only 15-second heartbeats that can take a long time. Waiting for a publication
does not count as a stall.
Reopen with `Last-Event-ID: <received ID>` to resume after a retained event;
omit the header for live-only delivery. The ID of the frame just before the
oldest retained one still resumes losslessly; unknown or older IDs return 400. The mock does not automatically create polled log
entries from events.

## Log services

A profile's system `LogService` behaves like a BMC's system event log rather
than an append-only list:

- It is bounded. The service reports `MaxNumberOfRecords` (512 unless the
  profile says otherwise) and `OverWritePolicy: WrapsWhenFull`; when full, the
  oldest entry is dropped and every remaining `Id` keeps its value.
- Ids are monotonic until the log is cleared. `POST
  .../LogServices/EventLog/Actions/LogService.ClearLog` empties the log and
  numbers the next entry `0` again, so a consumer keyed on `Id` alone sees new
  records under old ids, as it would on hardware.
- The Dell R750 profile pages its entries fifty at a time (see
  [Query parameters](#query-parameters)); a client that reads only the first
  page sees only the oldest entries. BlueField profiles serve their logs
  unpaged.
- Lifecycle entries carry `Created` at one-second resolution, `MessageId`,
  `Severity`, and `Links.OriginOfCondition`, matching the Event published for
  them. A client resuming from the newest entry it holds asks for
  `$filter=Created gt 2026-02-12T02:06:58Z`, the `Created` of that entry.

## Query parameters

One layer serves the collection query parameters of DSP0266 section 7.3 on
every resource collection, in the order the specification gives: `$filter`,
then `$skip` and `$top`, then `$expand`.

- `$filter` compares a member's properties (`Severity`, `Status/Health`,
  `Links/OriginOfCondition/@odata.id`) with `eq`, `ne`, `gt`, `ge`, `lt`, or
  `le` against a `'quoted string'`, a number, `true`, `false`, `null`, or a
  bare RFC 3339 instant, combined with `and`, `or`, `not`, and parentheses.
  A string property compares as an instant or a number when the literal is
  one and the property reads as one, so `Created gt 2026-02-12T02:06:58Z`
  works across offsets and `Id gt 12` works although `Id` is a string.
  Members served as references are judged by the resource they point at and
  stay references; a member whose resource cannot be read is left out and
  logged. The service root advertises `ProtocolFeaturesSupported.FilterQuery`.
- `Members@odata.count` is the number of members after `$filter`. `$skip`
  and `$top` page those; a collection with a page size of its own (the Dell
  R750 event log) pages at that size even without `$top`, and `$top` may
  shrink such a page but not grow it. `Members@odata.nextLink` repeats every
  option of the request with `$skip` advanced.
- `$expand=.($levels=N)` and `$expand=*` inline the members of the page.
  `$expand` is not advertised in `ProtocolFeaturesSupported`.

Misuse is answered with the Base registry messages the specification names,
in the error envelope's `code` and `@Message.ExtendedInfo`: any other `$`
parameter is a 501 `QueryParameterUnsupported`; a collection option on a
resource that is not a collection is a 400 `QueryNotSupportedOnResource`; a
`$skip` or `$top` that is not an integer, or a `$filter` the grammar does not
cover, is a 400 `QueryParameterValueFormatError`; a negative `$skip` or a
`$top` below 1 is a 400 `QueryParameterOutOfRange`. Parameters without a `$`
are ignored.

## Standalone state persistence

Use `--state-file PATH` with the libvirt backend to preserve BMC
accounts, BIOS overrides, boot policy, and manager/host-interface overrides
across restarts. Live power observations and sessions are not restored. Each BMC needs its own file and a single writer; the
parent directory must exist. Snapshots contain plaintext credentials and are
atomically replaced with owner-only permissions. Missing files are initialized
from the profile; corrupt, unsupported, or unreadable state fails startup.
Internal, archive and IPMI simulation modes do not support this option.

The libvirt application's `state_refresh_indication()` sends a notification
to the existing libvirt actor. The actor exports current state and writes only when
it differs from the last saved snapshot. Storage failures are retried without
rejecting changes already applied in memory. Abrupt termination can lose
unsaved changes; SIGINT and SIGTERM trigger a final save. There is no periodic
state polling. Callers changing state through internal APIs must indicate the
change themselves.

Embedded callers such as machine-a-tron use `BmcState::persisted()` and
`restore_persisted()` inside their own persistence lifecycle. `snapshot_source()`
provides weak access without retaining the BMC or depending on its callback
type. `persistence::atomic_write()` accepts bytes independently of snapshot
format. The library does not attach file storage or actors to resource state.

Snapshot readers ignore unknown fields so additive changes within a format
version do not prevent rollback. Missing required fields, invalid account
identities, mismatched system/manager identities, and unsupported versions remain
errors. Older account-only snapshots retain profile defaults for resource settings. MAT snapshots use
`bmc_state`; the old `bmc_accounts` format is not migrated.
Start with fresh persisted state when upgrading from that format.

### DPU BFB recovery

A BMC snapshot keeps the installed DPU image and retained BFB tasks together.
A restored unfinished task keeps its ID and resumes its restart. New tasks use a
fresh random epoch after each restore, so stale snapshots do not rewind the
current allocator. A non-DPU BMC rejects a `DPU_OS` update target with HTTP 404.
`PersistedBmcState::dpu_os_status()` supplies the saved disk to the MAT actor
integration, which must prefer it over a separately sampled OS observation.

Recovery replays the last captured install and its restart. If that install
completed after capture, restore can restart the DPU again and reapply the captured
image. Requests accepted after the last save are not recovered. Snapshots without
`dpu_update`, including snapshots from before this format, cannot recover pending
BFB installs on the first upgrade. Unremembered task IDs return HTTP 404.

## Switch NVOS endpoint

An NVLink switch tray has two management addresses: its BMC, which the
profiles above serve over Redfish, and its NVOS, which serves the NVUE REST
API. For the `nvidia_switch_n5700_ld` and `nvidia_switch_nd5200_ld` profiles,
`bmc_mock::nvue_router` returns a second router to serve at the switch's NVOS
address, and the `NvueState` behind it. machine-a-tron mounts it at the
switch's NVOS lease on the same listener as the BMCs.

The router answers `GET` on the `/nvue_v1/` trees NICo reads, with HTTP Basic
auth as the NVOS administrator: user `admin` (`DUMMY_NVOS_USERNAME`) with
password `factory_password` (`DUMMY_FACTORY_PASSWORD`) until a password
rotation changes it through `NvueState::set_password`. The responses
follow a capture from an N5400_LD switch tray running NVOS 25.02.4463: the
sensor roster, thresholds and application versions are that tray's, while the
NVLink port count, the partition and the management MAC address are the
simulated switch's.

| Path | Content |
| --- | --- |
| `/nvue_v1/system` | Hostname, status, uptime, release and the health summary |
| `/nvue_v1/system/version` | Image build id, base OS and kernel |
| `/nvue_v1/system/health` | Overall status, status LED, and the components with an issue |
| `/nvue_v1/system/reboot/reason` | The last reboot's reason, time and user |
| `/nvue_v1/platform` | System type, part number, serial number and port layout |
| `/nvue_v1/platform/environment` | Every fan, temperature sensor and LED with its state and type; the `STATUS` LED turns amber on a leak or an unhealthy system |
| `/nvue_v1/platform/environment/fan` | Fans `FAN<tray>/<fan>` with current, minimum and maximum speed and airflow direction |
| `/nvue_v1/platform/environment/led` | LED colors |
| `/nvue_v1/platform/environment/temperature` | Temperature sensors with the current reading and the thresholds each has |
| `/nvue_v1/platform/environment/leakage` | Leak sensors `LEAKAGE-<n>`, `ok` or `leak` |
| `/nvue_v1/interface` | NVLink ports `acp1` onwards, fabric management ports `fnm1` and `fnm2`, `eth0` and `lo` |
| `/nvue_v1/cluster/apps` | The NMX controller and telemetry applications |
| `/nvue_v1/sdn/partition` | The NVLink partitions, keyed by id |

The mock ignores `rev` and always returns operational values, where real NVOS
answers `{}` when asked for `applied` on an operational-only tree. It also
ignores `filter_`, as NVOS 25.02 does, so `/nvue_v1/interface` lists every
interface regardless of the type filter. It ignores `include` and `omit`, so
responses carry every field. A request without valid credentials gets the
switch's nginx `text/html` 401 page with
`WWW-Authenticate: Basic realm="nvueapi"`, and `HEAD` or `OPTIONS` gets its
403 page. Any other path under `/nvue_v1/` is 404 and any other method on a
served path is 405, as `application/problem+json`. Voltage sensors, PSUs,
interface counters, configuration writes, revisions and `nv config apply` are
not modeled.

`NvueState` lets a simulator change what the switch reports: `set_leak_state`
wets or dries a leak sensor, `set_system_health` sets the overall status and
the issue table, and `set_password` follows an NVOS password rotation. The
same fault-injection rules as the switch's BMC apply, through the management
endpoints served at the NVOS address.

The standalone `bmc-mock` binary does not serve an NVOS endpoint; the switch
profiles are reachable through machine-a-tron and the in-process test support.
