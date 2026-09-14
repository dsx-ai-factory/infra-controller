# bmc-mock

`bmc-mock` is a standalone HTTPS Redfish simulator. Its optional libvirt mode
maps one `bmc-mock` process or container to one libvirt domain, matching the
real-world relationship between a Redfish endpoint and its host.

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
Confirm those targets are unused with `virsh domblklist DOMAIN --details` before
inserting media. HTTP or HTTPS ISO URLs must be reachable from the host running
the QEMU process. File paths must exist in that host's filesystem.

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
not affect the host VM and intentionally does not expose libvirt virtual media.

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
- The Dell R750 profile pages its entries fifty at a time, with
  `Members@odata.count` giving the whole log and `Members@odata.nextLink`
  carrying `$skip` to the next page; `$top` may shrink a page but not grow it.
  A client that reads only the first page sees only the oldest entries.
  BlueField profiles serve their logs unpaged.
- Lifecycle entries carry `Created` at one-second resolution, `MessageId`,
  `Severity`, and `Links.OriginOfCondition`, matching the Event published for
  them.
