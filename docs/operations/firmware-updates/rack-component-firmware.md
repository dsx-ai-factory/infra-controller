# Rack and Tray Firmware Updates

Firmware updates for racks, compute trays, NVSwitches, and power shelves are
explicit operations. NICo does not start them from the host or DPU drift
detectors described elsewhere in this guide. A Provider Admin submits a REST
request, and NICo Flow creates one or more asynchronous tasks to sequence the
work.

Use this path when an operator needs Flow to update a rack or selected tray.
For desired-state updates on a conventional managed host, use
[Host firmware updates](host-firmware.md) instead.

## How the request runs

The REST response confirms that the task was created; it does not mean that
the firmware update has completed.

```mermaid
flowchart LR
    A["Provider Admin submits rack or tray request"] --> B["Flow resolves targets and an operation rule"]
    B --> C["Wait for affected hosts to be ready"]
    C --> D["Start update through NICo Core"]
    D --> E["Component backend applies firmware"]
    E --> F["Flow polls component status"]
    F --> G{"Terminal result?"}
    G -- No --> F
    G -- Success --> H["Task succeeds"]
    G -- Failure or timeout --> I["Task fails with a report"]
```

Before dispatching a disruptive operation, Flow checks persisted component
readiness. A compute-tray update checks the targeted hosts. An NVSwitch or
power-shelf update checks the hosts in the owning rack, because those devices
can affect every tenant in that rack. Flow waits for up to 30 minutes and
checks every 5 seconds by default.

If readiness information is missing, the current implementation logs a warning
and allows the operation to proceed. Do not treat the readiness check as a
substitute for confirming the maintenance scope and tenant impact.

## Choose the endpoint

All endpoints require the Provider Admin role. `siteId` is required in every
request body and must identify a site owned by the provider organization.

| Scope | Endpoint | Selection |
|---|---|---|
| One rack | [`PATCH /v2/org/{org}/nico/rack/{rack-id}/firmware`](api:PATCH/v2/org/{org}/nico/rack/{id}/firmware) | The rack in the URL. |
| Several or all racks | [`PATCH /v2/org/{org}/nico/rack/firmware`](api:PATCH/v2/org/{org}/nico/rack/firmware) | Optional `filter.names`. No filter means every rack in the site. |
| One tray | [`PATCH /v2/org/{org}/nico/tray/{tray-id}/firmware`](api:PATCH/v2/org/{org}/nico/tray/{id}/firmware) | The tray in the URL. |
| Several or all trays | [`PATCH /v2/org/{org}/nico/tray/firmware`](api:PATCH/v2/org/{org}/nico/tray/firmware) | Optional `filter`. No filter means all compute, NVSwitch, and power-shelf trays in every site rack. |

The batch tray filter supports:

| Filter | Meaning |
|---|---|
| `rackId` or `rackName` | Select trays in one rack. These fields are mutually exclusive. |
| `type` | Select exactly `Compute`, `NVSwitch`, or `PowerShelf`. |
| `componentIds` | Select component IDs of the specified `type`; `type` is required. |
| `ids` | Select tray UUIDs. |
| `slotId` | Select a rack slot; requires `rackId` or `rackName`. |

Rack selection cannot be combined with `ids` or `componentIds`. Prefer a
single-rack or single-tray request for the first update of a new firmware
bundle. An unfiltered batch request can affect the entire site.

## Describe the update

The request supports these controls in addition to `siteId`:

| Field | Purpose |
|---|---|
| `version` | Optional firmware input. Flow extracts component-specific values from layered JSON. A non-layered value that contains at least one non-whitespace character is forwarded unchanged. For a rack-scale RMS component with an omitted, null, empty, or whitespace-only value, Core fetches the owning rack profile's `firmware_object.url`. See [Choose the firmware object format](#choose-the-firmware-object-format). |
| `targets` | Optional component subset for tray requests. Rack handlers do not forward this field, so do not send it with a rack request. |
| `ruleId` | Pins the task to a custom Flow operation rule. When omitted, Flow resolves a rule and falls back to its built-in firmware rule. |
| `overrideReadinessCheck` | Bypasses Flow's readiness gate and tells Core to bypass its state controller where supported. Use only during supervised maintenance after tenant impact has been accepted. |
| `overrideVersionCheck` | Defaults to `false`. Requests an update without version-based skip or downgrade checks; enforcement depends on the backend. Does not bypass readiness checks. |
| `authenticationData` | Optional firmware-download credentials, shared or scoped by component type. See [Firmware authentication](#firmware-authentication). |

`version` carries the target firmware input. Its representation is a contract
between the caller and the component backend selected for the target, so the
REST API does not assign it one universal schema. Flow preserves the string
except when it unwraps the optional per-component-type mapping described below;
the selected component manager or its backend validates and interprets the
value.

When using the rack profile's desired firmware, each Core firmware request must
target a single rack. Flow splits rack, NVLink domain, and tray batch requests
into one task per rack. The built-in firmware rule batches each component type
within that rack's task. Batch requests can therefore omit `version` and use
each rack profile's desired firmware for rack-scale RMS components. An empty
or whitespace-only value for a selected component type in layered input uses
the same per-rack resolution.

The REST response remains asynchronous when `version` is omitted. Core resolves
the desired firmware object when the Flow task reaches each rack-scale RMS
component. A missing rack assignment, rack profile, or `firmware_object` source,
or a failed fetch, empty response, or invalid JSON response, fails the Core
firmware activity and the Flow task.

Non-RMS compute updates retain their existing empty-version behavior.

### Choose the firmware object format

Flow defaults to the `nico` component managers, which route updates through
Core's Component Manager. For RMS-backed compute, NVSwitch, and power shelf
updates, an explicit override requires the complete SOT firmware-object JSON
document produced by the firmware release process, serialized as a string.

For compatibility, when Flow's compute component manager is explicitly set to
[`nicolegacy`](../../configuration/flow-component-manager.md#selecting-the-compute-implementation),
use a compute tray endpoint without `version` or `targets` for on-demand host
updates. Core selects the bundle from the configured
[host firmware catalog](configuration.md#host-firmware-catalog).

#### SOT firmware-object JSON

RMS firmware is described by a source-of-truth (SOT) firmware object: a JSON
document containing the bundle identity and the artifacts RMS must apply. The
document comes from the platform's firmware release process; it is distinct
from the [host firmware catalog](configuration.md).

A SOT export has the following structure. This abbreviated example documents
the field hierarchy; it is not valid firmware-update input. Always submit the
complete document produced by the release process.

```json
{
  "ProductName": "ExampleRackSystem",
  "Milestones": [
    {
      "Name": "example-release",
      "State": "Onboarded",
      "BoardSKUs": [
        {
          "Name": "Example-Switch-Tray",
          "Type": "Switch Tray",
          "Components": {
            "Software": [],
            "Firmware": [
              {
                "Component": "BMC+CPLD",
                "Version": "1.2.3",
                "Type": "Prod",
                "FileNames": ["switch-firmware.fwpkg"],
                "Locations": [
                  {
                    "Location": "/firmware/example/switch-firmware.fwpkg",
                    "LocationType": "FILE",
                    "PackageName": "",
                    "Type": "Firmware",
                    "FileName": "switch-firmware.fwpkg"
                  }
                ],
                "SubComponents": [
                  {
                    "Component": "BMC",
                    "Version": "1.2.3",
                    "Type": null
                  }
                ]
              }
            ]
          }
        }
      ]
    }
  ]
}
```

To override the rack profile's desired firmware object, serialize the complete
JSON document into the REST `version` string.

When all selected component backends use RMS, `version` can hold one shared
SOT document. No additional flag is required. The document must
contain the board SKUs and artifacts needed by every component type selected
by the operation rule. Its decoded shape is:

```text
{
  "ProductName": "ExampleRackSystem",
  "Milestones": [{
    "Name": "example-release",
    "BoardSKUs": [
      {"Type": "Compute Node", "Components": { ... }},
      {"Type": "Switch Tray", "Components": { ... }},
      {"Type": "Power Shelf", "Components": { ... }}
    ]
  }]
}
```

The ellipses represent the complete component and artifact metadata from the
SOT export; they are not literal request content. Because this object has none
of the reserved top-level keys `compute`, `nvswitch`, and `powershelf`, Flow
passes the same serialized document unchanged to every component manager
selected by the operation rule.

For a rack request that needs a different value for each component type,
`version` can contain a layered JSON document with `compute`, `nvswitch`, and
`powershelf` keys. Flow extracts the relevant value before calling each
component manager. Each value must satisfy that component's backend contract.
For RMS-backed components, the decoded shape is:

```text
{
  "compute": {"ProductName": "ExampleComputeSystem", "Milestones": [ ... ]},
  "nvswitch": {"ProductName": "ExampleSwitchSystem", "Milestones": [ ... ]},
  "powershelf": {"ProductName": "ExamplePowerSystem", "Milestones": [ ... ]}
}
```

The ellipses stand for complete SOT documents in this RMS-specific example;
they are not literal request content. Flow forwards object values as raw JSON and
unquotes string values before forwarding them. The outer REST `version` field
remains a string in both the shared and layered forms.

If a layered document omits a component-type key, Flow skips every rule step
for that component type, including pre/post actions and later stages. Those
steps are reported as skipped. The component's targets remain available to
cross-component readiness checks in selected steps.

### Firmware authentication

`authenticationData` accepts exactly one of `shared` (an opaque credential
string) or `perComponent` (an object with optional `compute`, `nvswitch`, and
`powershelf` credential strings). Unknown keys are rejected at both levels.
Omission or `null` supplies no credentials; empty strings or an empty
`perComponent` object also supply none. Missing component entries do not
inherit another type's credential. Do not encode component mappings inside
`shared`. Non-empty credentials are not supported for DPU-only updates or by
the legacy NICo compute firmware controller.

For RMS-backed updates, the selected credential is the artifact access token.
Without a token, Core sends `NOAUTH`. Non-empty credentials require Flow's
[persisted firmware authentication](../../architecture/flow.md#persisted-firmware-authentication)
encryption configuration. Keep credentials out of shell arguments and logs.

## Submit an update

This rack-scale RMS request updates only the BMC and BIOS targets on one
compute tray:

```json
{
  "siteId": "2b88bb63-9a21-4bad-b113-68a54aa6e3dd",
  "version": "<complete SOT firmware-object JSON serialized as a string>",
  "targets": ["bmc", "bios"]
}
```

The response contains the task IDs to monitor:

```json
{
  "taskIds": ["c5e00a88-9b42-4e2b-a237-63e787f698ef"]
}
```

To update all racks named `A01` and `A02`, submit a batch rack request:

```json
{
  "siteId": "2b88bb63-9a21-4bad-b113-68a54aa6e3dd",
  "filter": {
    "names": ["A01", "A02"]
  },
  "version": "<complete or layered firmware target serialized as a string>"
}
```

## Default sequencing

When no site-specific rule or `ruleId` applies, Flow uses this built-in rule:

| Stage | Component type | Status polling | Attempts |
|---|---|---|---|
| 1 | Compute | Every 2 minutes, for up to 45 minutes | 2 |
| 2 | NVSwitch | Every 2 minutes, for up to 45 minutes | 2 |

The stages run in order. A failed or timed-out stage fails the task. A stage
that has no matching components is reported as skipped.

The built-in rule deliberately excludes power shelves and does not perform an
AC power cycle after flashing. Use an approved custom operation rule for power
shelves. If firmware activation requires a power cycle, submit the appropriate
power-recycle task separately or include it in a custom rule. Refer to the
Flow [Operation Rules Guide](../flow/operation-rules.md).

## Component behavior

### Compute trays

Supported `targets` are:

`bmc`, `bios`, `cec`, `nic`, `cpld_mb`, `cpld_pdb`, `hgx_bmc`,
`combined_bmc_uefi`, `gpu`, and `cx7`.

Omitting `targets` asks Core to update all components represented by the
selected firmware bundle. It does not include DPU reprovisioning.

`dpu` is a special, explicit-only compute target. NICo first submits any
compute-tray update, then reprovisions the DPU on each selected host serially.
The request's `version` is ignored for the DPU branch; its target comes from
site configuration. Use `targets: ["dpu"]` for a DPU-only request. Refer to
[Assigned hosts and operator requests](dpu-firmware.md#assigned-hosts-and-operator-requests)
before using this path.

Core routes conventional standalone machines through the managed-host firmware
workflow and rack-scale compute trays through the configured rack state
controller or component backend. Consequently, a single REST shape can start
different internal workflows depending on the hardware model.

For rack-scale RMS updates, Core resolves an omitted version from the owning
rack profile before state-controller or direct-backend dispatch.

### NVSwitches

Supported `targets` are `bmc`, `cpld`, `bios`, and `nvos`. Omitting `targets`
passes an empty component list to Core, which means all supported switch
components for the selected backend.

When `version` is omitted, Flow calls Core with an empty target. Core resolves
the owning rack profile's desired firmware object before starting an RMS update.
Non-RMS direct updates skip dispatch when every switch matches a configured
desired firmware entry; otherwise Core forwards the empty target to the backend.

### Power shelves

Supported `targets` are `pmc` and `psu`. If `targets` is omitted, the current
Flow component manager updates only `pmc`.

Power shelves are not present in the built-in firmware rule, so a power-shelf
tray request needs an operation rule that contains a `PowerShelf` firmware
step. For RMS updates, Core resolves an omitted version from the owning rack
profile before state-controller or direct-backend dispatch.

## Monitor and cancel tasks

Use [Retrieve a Task](api:GET/v2/org/{org}/nico/task/{id})
to read each returned task ID until it reaches `Succeeded`, `Failed`, or
`Terminated`:

```http
GET /v2/org/{org}/nico/task/{task-id}?siteId={site-id}
```

The task report records each rule stage and component step as `pending`,
`running`, `completed`, `failed`, or `skipped`. On failure, inspect both the
top-level `message` and the report's stage or step `error`.

You can also
[list tasks for a rack](api:GET/v2/org/{org}/nico/rack/{id}/task)
or [list tasks for a tray](api:GET/v2/org/{org}/nico/tray/{id}/task).

The `activeOnly=true` query parameter restricts the result to non-terminal
tasks, and the `includeReport=true` query parameter includes the stage and
step report.

```http
GET /v2/org/{org}/nico/rack/{rack-id}/task?siteId={site-id}&activeOnly=true&includeReport=true
GET /v2/org/{org}/nico/tray/{tray-id}/task?siteId={site-id}&activeOnly=true&includeReport=true
```

[Cancel a Task](api:POST/v2/org/{org}/nico/task/{id}/cancel)
is best effort. It terminates a pending, running, or waiting task, but it cannot
undo firmware work already accepted by a hardware backend. Completed and
failed tasks cannot be cancelled.

```text
POST /v2/org/{org}/nico/task/{task-id}/cancel

{"siteId":"<site-id>"}
```

After cancellation or failure, inspect component status before retrying. Do not
assume that every component remained on its previous version.

## Inspect Core component status

The `component-manager` commands call NICo Core directly. Use them to inspect
the backend's view of a running REST task or for platform-specific recovery.
They do not create a Flow task or apply its operation rule.

```sh
nico-admin-cli component-manager get-firmware-update-status rack \
  --rack-id <rack-id>

nico-admin-cli component-manager get-firmware-update-status compute-tray \
  --machine-id <machine-id>

nico-admin-cli component-manager get-firmware-update-status switch \
  --switch-id <switch-id>

nico-admin-cli component-manager get-firmware-update-status power-shelf \
  --power-shelf-id <power-shelf-id>

nico-admin-cli component-manager get-firmware-versions switch \
  --switch-id <switch-id>
```

Core also exposes direct update commands. Prefer the REST workflow for normal
rack operations because it provides readiness checks, ordering, task reports,
and cancellation. Direct commands require the operator to provide those
safeguards.

For RMS-backed compute trays, switches, or power shelves, pass a SOT file rather
than embedding it on the command line:

```sh
nico-admin-cli component-manager update-firmware compute-tray \
  --machine-id <machine-id> \
  --sot-json-file ./compute-firmware-object.json

nico-admin-cli component-manager update-firmware switch \
  --switch-id <switch-id> \
  --sot-json-file ./switch-firmware-object.json \
  --component bmc,cpld,bios,nvos

nico-admin-cli component-manager update-firmware power-shelf \
  --power-shelf-id <power-shelf-id> \
  --sot-json-file ./power-shelf-firmware-object.json \
  --component pmc,psu
```

Run `nico-admin-cli component-manager update-firmware --help` for the complete
command options. Refer to the
[rack state machine](../../architecture/state_machines/rackstatemachine.md) for
lower-level execution details.

## Troubleshooting

| Symptom | Check |
|---|---|
| No work starts after the REST response | Read the returned task. It may be waiting at the readiness gate or for an earlier rule stage. |
| Task fails after about 30 minutes | Inspect the error for component IDs blocked by the readiness gate. Confirm tenant state and the persisted component operation status. |
| Stage times out | Check Core and backend status. The built-in firmware rule polls for 45 minutes per attempt; a backend job can still be running when Flow times out. |
| Rack-scale update fails before dispatch with an omitted or empty `version` | Confirm that the failed Core firmware request targets one rack. Verify that the rack profile has a configured, reachable `firmware_object.url` that returns a JSON object. |
| Rack-scale update rejects an explicit `version` | Confirm that `version` contains a valid SOT JSON object, serialized as a string, and that the selected firmware-download credential can access the referenced artifacts. |
| Power-shelf request succeeds without updating a shelf | Confirm that the resolved operation rule contains a `PowerShelf` step. The built-in rule excludes power shelves. |
| Firmware was flashed but is not active | Determine whether the platform requires an AC cycle. The built-in firmware rule does not include one. |
| Retry begins from an uncertain state | Inspect per-component status and inventory first. A Flow task failure or cancellation does not roll hardware back. |

Set `overrideReadinessCheck: true` only after diagnosing a readiness block and
confirming that the affected hardware is in a supervised maintenance window.
The override bypasses a tenant-safety guard and may also send the update
directly to the component backend instead of through Core's state controller.
