# Boot Interfaces and DPU Policies <Badge intent="info">v2.0</Badge>

This guide explains how NICo decides **which interface a host boots from**, how a host's **DPUs are managed**, and how operators configure both through the Expected Machines table. It is the deep companion to [Ingesting Hosts](ingesting-hosts.md): that page covers the end-to-end ingest flow and the basic `expected_machines.json`; this page covers the per-host and per-interface knobs (`dpu_policy`, `interfaces`), **what the defaults do when you set nothing**, and how a boot device is chosen and applied behind the scenes.

For the DHCP and network-segment substrate these knobs sit on (how a relay's `giaddr` maps to a segment), see [IP and Network Configuration](ip-and-network-configuration.md).
For interface roles and IP allocation policies, see
[Configure Expected Machine Interfaces](expected-machine-interfaces.md).

> **Who should read this.** Operators configuring hosts for ingestion, and anyone debugging "why did this host boot from *that* interface?" **Most hosts need no configuration here** — the defaults handle the common managed-DPU case. Reach for the knobs in [Section 2](#2-configuring-via-expected-machines-and-the-defaults) and [Section 3](#3-scenarios) only for `nic`, `ignore`, or integrated-NIC hosts. [Sections 6](#6-behind-the-scenes-how-a-boot-device-is-chosen-and-set)–[7](#7-the-boot-interface-data-model) explain the machinery when you need to trace a problem.

---

## 1. The mental model: two independent axes

Historically, several related decisions were conflated into "what kind of host is this." Keep them apart:

| Axis | Question | Controlled by |
|---|---|---|
| **DPU management** | Does NICo manage this host's DPUs (upgrade them, run agents, and attach them to the ManagedHost)? | `dpu_policy` |
| **Redfish boot target** | Which NIC should the BMC boot? | the durable desired boot-interface target, initialized from discovery/Expected Machine data |
| **Admin network identity** | Which owned NIC supplies the host's admin address and network identity? | the managed `primary_interface` selection; modern setters update it together with the desired boot target |

A "normal" host aligns them (managed DPU + boot and admin identity through that DPU). But DPU management is independent: you can keep a host's DPUs **managed** and still boot it from a plain **integrated NIC**. Modern managed-host setters keep the boot target and admin network identity aligned; the legacy explored-endpoint action can change only the desired target.

The policy is also separate from the card's current hardware state. NICo models
operator intent as `HostDpuPolicy::{Manage, Nic, Ignore}` and the mode
reported by a BlueField as `BlueFieldOperatingMode::{Dpu, Nic}`. Existing
protobuf and Redfish boundaries retain the legacy `NicMode` name for
compatibility, but model code does not use that name for observed state.

### Network segment types

A host's management network lives on one of a few segment types. Which one depends on **how the host boots**:

| Segment type | What it is | Used when |
|---|---|---|
| **Admin** | A **DPU-served overlay**. A DPU in DPU mode runs an on-board DHCP server and hands the host OS its admin IP over the overlay; the physical fabric never sees it (the DPU is a VTEP). | The host boots **through a DPU**. |
| **HostInband** | An ordinary NIC straight to the physical fabric. The host gets its IP from central NICo DHCP (`nico-dhcp`), and the switch port is configured differently. This is also the segment Flat-VPC allocation keys off. | The host boots through a **plain NIC** — an integrated NIC, or a DPU in NIC mode. A zero-DPU host BMC may share this segment with the host OS. |
| **Underlay** | The DPU's *own* IP (its loopback/VTEP) and the conventional OOB/BMC management network. | For DPU self-addressing, DPU BMC/OOB, and the usual isolated host-BMC topology. |
| **Tenant** | Tenant workload networks. | After a host is assigned to a tenant. |

**Rule of thumb:** the host-OS segment follows the boot interface's mode — boot via DPU ⇒ **Admin**; boot via a plain NIC ⇒ **HostInband**. A non-DPU NIC is never on the Admin segment, because Admin *is* the DPU overlay. The supported shared host-BMC topology is documented in [IP and Network Configuration](ip-and-network-configuration.md#15-shared-hostinband-for-a-host-bmc-and-host-os).

### The boot and primary interface

The managed `primary_interface` row selects the host's admin network identity and supplies the ordinary boot default. The machine-scoped `machine_boot_interfaces` row separately persists the desired Redfish target that `Ready` convergence applies. Operator setters update both in one transaction, so they normally remain aligned, but they are different facts: desired intent can exist from a prediction before DHCP creates an owned row, and a pending target can temporarily differ from the current primary/default view.

A complete boot interface is a `(MAC, Redfish interface id)` pair — the MAC identifies the NIC on the wire; the Redfish id lets NICo set the boot order on the BMC. NICo calls that pair a `MachineBootInterface`; a desired target may remain MAC-only until site-explorer learns one unambiguous Redfish id.

---

## 2. Configuring via Expected Machines and the defaults

Boot and DPU configuration is **declarative**: you describe the host in the Expected Machines table, and site-explorer + the machine-controller make it so during ingestion. For the table's basics (schema, `replace-all` upload, credentials), see [Ingesting Hosts → Add Expected Machines](ingesting-hosts.md#add-expected-machines-table). This section covers only the boot/DPU fields.

### If you set nothing (the default)

**Most hosts with DPU hardware need zero boot/DPU configuration.** Outside rack-manager deployments, when neither the host nor the site sets `dpu_policy`, and the host has no `interfaces` declaration:

- The effective `dpu_policy` resolves to **`manage`** — NICo ingests and manages the host's DPUs, and the host boots through its primary DPU on the **Admin** network.
- Site-explorer **auto-selects the boot interface**: it uses the lowest UEFI PCI path when every matching DPU host interface in the Redfish report has one. Otherwise, it orders the DPUs by their Redfish serial numbers without regard to case and uses the first interface in that order.
- The host's IP comes from whichever segment its DHCP relay lands in (see [IP and Network Configuration](ip-and-network-configuration.md)).

So a standard DPU host is handled entirely by defaults. A host without DPU hardware must instead use `dpu_policy: ignore` and declare a primary HostInband NIC as described in [3.2](#32-zero-dpu-host-no-dpu-hardware). The knobs below exist for the hosts that *don't* fit the standard-DPU mold.

`rack_management_enabled` is a separate deployment-mode setting, not another
DPU policy, and it does not override this resolution. Rack-manager deployments
that operate DPUs as NICs must also set the site-wide `dpu_policy` (or each
host's policy) to `nic`. If rack management is enabled while both policy
levels remain unset, the effective policy is still `manage` and the Admin-network
default above still applies.

### `dpu_policy`

| JSON/TOML value | CLI value | Meaning |
|---|---|---|
| `manage` (site/effective default) | `manage` | DPU hardware is expected and managed by NICo. With no declared primary host NIC, the default boot interface is the primary DPU host-PF on the Admin overlay; a declared integrated primary can instead boot on HostInband. A host without DPU hardware must use `ignore` and declare a primary HostInband NIC. A per-host `manage` declaration inherits the site policy rather than overriding it. |
| `nic` | `nic` | DPU hardware is present but should operate as a **plain NIC**. Site-explorer explores it but does **not** link or manage it; the host boots on **HostInband**. |
| `ignore` | `ignore` | NICo does not configure or attach DPU hardware. Use this for a host without DPUs or when installed DPUs should be intentionally ignored; the host boots through a plain NIC on **HostInband**. |

**Resolution order:** per-host `nic` and `ignore` policies override the site. For backward compatibility, per-host `manage` (like an omitted per-host policy) defers to the site-wide `[site_explorer] dpu_policy`; if the site policy is also unset, the result is `manage`.

Compatibility declarations remain accepted when deserializing configuration and admin JSON input: the previous `dpu_policy` value `use_as_nic` remains accepted, and the legacy `dpu_mode` key with values `dpu_mode`, `nic_mode`, and `no_dpu` maps to `manage`, `nic`, and `ignore`, respectively. The admin CLI likewise accepts the previous `use-as-nic` value and legacy `--dpu-mode dpu-mode|nic-mode|no-dpu` forms, but new automation should use `--dpu-policy manage|nic|ignore`.

The Forge RPC intentionally retains `ExpectedMachine.dpu_mode` (field 16,
`DpuMode`) as its stable compatibility surface. Direct gRPC clients continue to
send `DPU_MODE`, `NIC_MODE`, or `NO_DPU`; NICo translates those values at the
RPC boundary to the internal `manage`, `nic`, and `ignore` policies.
`HostDpuPolicy` and `dpu_policy` are model, configuration, admin-CLI, and admin
JSON vocabulary rather than Forge protobuf symbols. Responses translate
non-default policies back through `dpu_mode`; the default `manage` policy might
leave that field unset.

### Expected Machine Interface Declarations

The optional `interfaces` array declares host and DPU interfaces. Each
`ExpectedInterface` entry identifies an interface by MAC address and can set its
role, IP allocation policy, segment guard, and primary status.

For boot selection, declare an entry with `role: "host"` and
`primary: true`. Only a `host` entry can set `primary`, and at most one entry
per Expected Machine can set it to `true`. If `role` is omitted, it defaults to
`host`.

See [Configure Expected Machine Interfaces](expected-machine-interfaces.md)
for the full field reference, all four roles, allocation policies, segment
selection behavior, and backward-compatible input aliases.

**Admin JSON** (an Expected Machine entry):

```json
{
  "bmc_mac_address": "C4:5A:B1:C8:38:0D",
  "bmc_username": "root",
  "bmc_password": "<bmc-password>",
  "chassis_serial_number": "SERIAL-1",
  "dpu_policy": "manage",
  "interfaces": [
    {
      "mac_address": "C4:5A:B1:C8:38:10",
      "role": "host",
      "primary": true,
      "network_segment_type": 3
    }
  ]
}
```

**CLI** (single host):

> **Security:** Values passed to `--bmc-password` can appear in shell history
> and process listings. Substitute credentials only in a protected
> administrative environment and follow your site's secret-handling policy.

```bash
nico-admin-cli -a <api-url> em add \
  --bmc-mac-address C4:5A:B1:C8:38:0D \
  --bmc-username root --bmc-password '<bmc-password>' \
  --chassis-serial-number SERIAL-1 \
  --dpu-policy manage \
  --interfaces '[{"mac_address":"C4:5A:B1:C8:38:10","role":"host","primary":true,"network_segment_type":3}]'
```

(`em` is the alias for `expected-machine`.)

Admin CLI manifests and inline CLI JSON use protobuf enum values for
`network_segment_type`. HostInband is `3`.

---

## 3. Scenarios

Concrete recipes for the cases beyond the default. All assume the rest of the Expected Machine entry (BMC credentials, serial) is filled in as usual.

### 3.1 Standard DPU host

With the site policy unset or set to `manage`, there is nothing to configure. DPUs are managed and the host boots through the primary DPU on Admin.

### 3.2 Zero-DPU host (no DPU hardware)

A plain server with one or more host NICs and no DPU. Declare `ignore` and mark the boot NIC primary:

```json
{
  "dpu_policy": "ignore",
  "interfaces": [
    { "mac_address": "AA:BB:CC:00:00:10", "role": "host", "primary": true, "network_segment_type": 3 }
  ]
}
```

The host boots from that NIC on HostInband and gets its IP from central NICo
DHCP. Its host BMC may use the same physical subnet/VLAN and HostInband segment;
see [Shared HostInband for a Host BMC and Host OS](ip-and-network-configuration.md#15-shared-hostinband-for-a-host-bmc-and-host-os).

### 3.3 DPU in NIC mode

The host has DPU hardware, but you want it treated as a plain NIC (not managed). Declare `nic`. Site-explorer still explores the DPU (and will issue the physical mode flip — see [3.5](#35-flipping-a-dpu-to-nic-mode)) but does not link it as a managed machine; the host boots HostInband:

```json
{
  "dpu_policy": "nic",
  "interfaces": [
    { "mac_address": "AA:BB:CC:00:00:20", "role": "host", "primary": true, "network_segment_type": 3 }
  ]
}
```

### 3.4 Boot an integrated NIC while keeping the DPUs managed

This is the case where the two axes genuinely diverge: the host has cabled, explorable DPUs you **want managed** (for the data plane), but you want the host OS to boot from a **plain integrated NIC** rather than through a DPU. First ensure the site-wide policy is unset or `manage`; a per-host `manage` declaration inherits the site and cannot override `nic` or `ignore`. Then leave the host policy unset (or explicitly set `manage`) and mark the integrated NIC primary on HostInband:

```json
{
  "dpu_policy": "manage",
  "interfaces": [
    { "mac_address": "AA:BB:CC:00:00:30", "role": "host", "primary": true, "network_segment_type": 3 }
  ]
}
```

NICo keeps the DPUs explored, linked, and underlay-addressed (running agents for the data plane), but the host boots from the integrated NIC. The DPU-backed admin links are kept but go **dormant** — the host's admin/boot path is the HostInband NIC.

> Previously this required selecting an unmanaged-DPU policy, which threw away DPU management to get integrated boot. The two are now decoupled.

### 3.5 Flipping a DPU to NIC mode

To change a host that's already ingested, first release any attached Instance,
then update its Expected Machine policy with `--dpu-policy` and force-delete it
with the Instance Type override so Site Explorer re-explores and applies the
new physical mode:

```bash
nico-admin-cli -a <api-url> em patch --bmc-mac-address <bmc-mac> --dpu-policy nic
nico-admin-cli -a <api-url> machine force-delete --machine <machine-id> --delete-interfaces --allow-delete-with-instance-type
```

See the [Force Delete playbook](../playbooks/force_delete.md) for the full re-ingest procedure. NICo preserves the host's boot-interface **Redfish id** across the deletion gap via the retained-boot-interface mechanism ([Section 7.4](#74-retained-boot-interfaces)), so the host can be re-targeted for boot before a fresh exploration completes. After a flip, you can re-apply the resolved boot interface with one click via **Restore Boot Interface** in the web UI ([Section 5](#5-web-ui)).

The mode change itself takes effect only across a power cycle: The queued `Mode.Set` stays staged on the DPU until the host power-cycles. Site-explorer issues that power cycle automatically on every vendor, trying the standard Redfish `PowerCycle` reset first and escalating to a cold `ACPowercycle` on platforms that refuse it (HPE, Lenovo, Supermicro, and GBx00 systems). Repeat cycles are rate-limited, so a host mid-flip can take a pass or two of exploration to converge. If both reset types are refused, or the DPUs still report the old mode after cycling, the host surfaces the `manual_power_cycle_required` pairing blocker and waits for an operator (refer to [Section 8](#8-verifying-and-troubleshooting)).

---

## 4. admin-cli and gRPC reference

All of these are **admin-only**; the Forge gRPC service enforces admin authorization. The `nico-admin-cli` commands are thin wrappers over the listed Forge RPCs.

### Expected machines

| admin-cli | Forge RPC | Purpose |
|---|---|---|
| `em add …` | `AddExpectedMachine` | Add one host (BMC creds, `--dpu-policy`, `--interfaces`, metadata). |
| `em show [--bmc-mac-address <mac>]` | `GetAllExpectedMachines` / `GetExpectedMachine` | List all, or show one. Add `-f json` to export. |
| `em update --filename <json>` | `UpdateExpectedMachine` | Full replacement of one entry from JSON. |
| `em patch --bmc-mac-address <mac> …` | `UpdateExpectedMachine` | Partial update (e.g. `--dpu-policy`), preserving other fields. |
| `em delete --bmc-mac-address <mac>` | `DeleteExpectedMachine` | Remove one entry. |
| `em replace-all --filename <json>` | (bulk) | Replace the entire table from a file. |
| `em erase` | (bulk) | Erase the entire table. |

### Boot interface / primary interface

| admin-cli | Forge RPC | Purpose |
|---|---|---|
| `managed-host set-primary-interface <host-id> <interface-id> [--force-reconcile] [--reboot]` | `SetPrimaryInterface` | Atomically make one owned interface primary, move the corresponding Admin address/network identity, and persist that exact desired boot target. Hosts with a DPU-backed Admin interface require the selected interface to be on the Admin segment. `--force-reconcile` opens a fresh reconciliation generation even when the interface is already selected; `--reboot` is a deprecated compatibility flag whose behavior depends on the server version. |
| `managed-host set-primary-dpu <host-id> <dpu-id> [--force-reconcile] [--reboot]` | `SetPrimaryDpu` | Deprecated compatibility command that selects the DPU's host-facing interface through the same primary/desired-state transaction. The reconciliation flags behave as they do for `set-primary-interface`. Prefer `set-primary-interface`, which takes a machine-interface ID rather than a DPU machine ID. |
| `boot-interface show <machine-id>` | `GetMachineBootInterfaces` | Show every store (managed, predicted, explored, retained), the effective owned pick, persisted desired target, selection source and decision time, desired/verified versions, observation kind/time, derived reconciliation state, active controller version/failure, and current-selection disagreement. Read-only. |
| `boot-interface candidates <machine-id>` | `GetMachineBootInterfaces` | List a machine's candidate boot NICs and the picks among them (`current`, `default`, `explored`), plus the desired selection source and decision time. An already-declared primary remains eligible regardless of segment; non-primary underlay rows are ineligible for the automatic fallback. Read-only. |
| `boot-interface set <machine-id> <interface> [--force-reconcile] [--reboot]` | `SetPrimaryInterface` | Run the same primary/Admin-identity/desired-state transaction as `set-primary-interface`, selecting by machine-interface UUID or by a MAC that matches exactly one managed row. Hosts with a DPU-backed Admin interface require the selected interface to be on the Admin segment. The reconciliation flags behave as they do for `set-primary-interface`. |
| `boot-override set <interface-id> [--custom-pxe <f>] [--custom-user-data <f>]` | `SetMachineBootOverride` | Override the iPXE script / cloud-init user-data served at boot. |
| `boot-override get <interface-id>` | `GetMachineBootOverride` | Show the current boot override. |
| `boot-override clear <interface-id>` | `ClearMachineBootOverride` | Revert to the default PXE/cloud-init. |

A server that supports `SetPrimaryInterfaceRequest.force_reconcile` treats `--force-reconcile` and deprecated `--reboot` as
requests for a new desired generation. Neither flag directly reboots the host or bypasses lifecycle eligibility. NICo
enqueues a Ready host immediately only when it has no instance; otherwise, the generation remains pending until the
machine controller can reconcile it. While old and new server versions run together, an old server ignores
`--force-reconcile`: changing the primary writes the Redfish boot order immediately, while selecting the current primary
fails. Legacy `--reboot` also restarts the host.

`GetMachineBootInterfacesResponse.Reconciliation.selection_source` is a `BootInterfaceSelectionSource` that records why
NICo selected the current boot interface. The interface can belong to a DPU operating in DPU or NIC mode, or it can be
an integrated or onboard NIC. This field does not describe DPU mode or reconciliation state. The admin CLI and web UI
treat a numeric value they do not recognize as `Unspecified`.

#### Boot interface selection sources

The admin CLI and web UI use the following display labels:

| Display label | Protobuf identifier | Meaning |
|---|---|---|
| `Unspecified` | `BOOT_INTERFACE_SELECTION_SOURCE_UNSPECIFIED` | The server did not provide a concrete value; this is a compatibility value on the wire, not a persisted selection source. |
| `ExpectedMachine` | `BOOT_INTERFACE_SELECTION_SOURCE_EXPECTED_MACHINE` | An `ExpectedMachine` declaration selected the interface. |
| `Operator` | `BOOT_INTERFACE_SELECTION_SOURCE_OPERATOR` | An operator selected the interface through an administrative write. |
| `RedfishUefiPci` | `BOOT_INTERFACE_SELECTION_SOURCE_REDFISH_UEFI_PCI` | Redfish UEFI PCI path ordering selected the interface. |
| `RedfishChassisId` | `BOOT_INTERFACE_SELECTION_SOURCE_REDFISH_CHASSIS_ID` | Reserved for [selection from Redfish chassis identity](https://github.com/NVIDIA/infra-controller/issues/5080). |
| `RedfishSerialNumber` | `BOOT_INTERFACE_SELECTION_SOURCE_REDFISH_SERIAL_NUMBER` | NICo selected the interface after ordering DPUs by their Redfish serial numbers. Comparison ignores case. A missing serial number is an empty key and sorts before a populated value; equal keys preserve discovery order. |
| `ScoutReportPci` | `BOOT_INTERFACE_SELECTION_SOURCE_SCOUT_REPORT_PCI` | Reserved for [selection from host PCI data reported by Scout](https://github.com/NVIDIA/infra-controller/issues/5083). |
| `LegacyUnknown` | `BOOT_INTERFACE_SELECTION_SOURCE_LEGACY_UNKNOWN` | NICo cannot recover why the interface was selected, including migrated rows and compatibility baselines for an existing Ready or Assigned host. |

`selection_updated_at` records when the selected MAC or its selection source last changed. Enriching the same MAC
with a Redfish interface ID preserves this time. It is omitted when NICo cannot recover the decision time, including
migrated rows and compatibility baselines for an existing Ready or Assigned host. It is independent of `observed_at`,
which records the latest persisted Redfish observation or rollout compatibility baseline;
`is_compatibility_baseline` distinguishes those cases.

During a rolling deployment, a server that predates selection source tracking can change the desired boot interface
without updating these fields. The source and decision time can describe the preceding selection until a writer that
records selection sources makes another selection.

Without `--force-reconcile` or its deprecated `--reboot` alias, selecting the current primary through
`set-primary-interface` is still meaningful when its recorded source is not already `Operator`: the first request records
`Operator` authority without opening a new Redfish reconciliation generation. Repeating that same explicit choice reports
that the operator already selected the primary interface. Either reconciliation flag instead opens a fresh generation.

> The explored-endpoint web view also exposes a legacy **Set First** action by MAC ([Section 5](#5-web-ui)). For an endpoint owned by a managed or predicted host, that request changes only the desired target; it does not promote an owned `machine_interfaces` row to primary. Prefer the managed-host action when an owned row should become both primary and desired. Unowned and DPU-owned endpoints apply Redfish directly. Under normal operation machine-controller sets a managed host's boot order automatically ([Section 6](#6-behind-the-scenes-how-a-boot-device-is-chosen-and-set)).

### Ingestion control

| admin-cli | Forge RPC | Purpose |
|---|---|---|
| `site-explorer remediation <bmc-ip> --pause` / `--resume` | `PauseExploredEndpointRemediation` | Pause/resume site-explorer's automatic remediation (and ingestion processing) for an endpoint. |
| `machine force-delete --machine <id> [--delete-interfaces] [--delete-bmc-interfaces] [--delete-bmc-credentials] [--allow-delete-with-instance-type] [--allow-delete-with-instance]` | `AdminForceDeleteMachine` | Remove a Machine from the database, bypassing the normal lifecycle. The Instance Type flag removes that association; the Instance flag implies it and removes the attached Instance control-plane record without first requesting a graceful workload shutdown. Force-delete cleanup may forcibly restart the host. |
| `managed-host show [--all \| <machine-id>]` | (query) | Inspect a host's current state, interfaces, and database primary selection. This does not prove that Redfish matches; use `boot-interface show` for reconciliation. |

---

## 5. Web UI

The NICo admin web UI (`/admin/…`) exposes the managed host's desired boot interface and a focused set of boot/ingestion actions. **There is no DPU-policy control in the UI** — change `dpu_policy` via Expected Machines (CLI/JSON) as in [Section 2](#2-configuring-via-expected-machines-and-the-defaults).

**View:**

- `/admin/machine` and `/admin/machine/{id}` — machine inventory and detail, including each interface's **primary indicator**, MAC, segment, and attached DPU ID. A managed host's **Desired Boot Interface** section shows its persisted target, selection source and decision time, desired and verified versions, observation time, derived reconciliation state, active controller version, failure (if any), and managed/predicted candidates.
- `/admin/dpu` and `/admin/dpu/versions` — DPU inventory, associated host, and version info (read-only).
- `/admin/expected-machine` — a status board of expected vs. unexpected machines, with tabs for **Completed / Unseen / Unexplored / Unlinked / Unexpected**. (Read-only; entries are defined via the CLI/JSON.)
- `/admin/explored-endpoint` — discovered BMC endpoints with their **preingestion state**, last-exploration latency, and errors.

**Act:**

- On a managed-host detail page, **Set desired interface** selects one exact managed interface and also moves the host's primary/admin network identity. **Use system default** persists the current unambiguous default once; it does not follow future default changes. Predicted candidates remain informational until DHCP creates an owned row. **Request reconciliation** preserves an initialized target while opening a fresh controller generation, and requires complete BMC data. These actions update desired state; they do not call Redfish from the web request.
- On an explored-endpoint page, **Set First** accepts a boot-interface MAC and **Restore Boot Interface** reuses the resolved target. If the endpoint belongs to a managed or predicted host, the request changes only the desired target for machine-controller; it does not promote an owned interface to primary. Prefer the managed-host action when both selections should change. Unowned and DPU-owned endpoints use the direct Redfish path because no managed-host state controller owns them.
- **Machine Setup** — prepare an endpoint for ingestion (optionally with a boot-interface MAC; Dell endpoints require it).
- Endpoint controls — **Re-Explore**, **Refresh**, **Clear Last Error**, **Pause/Resume Remediation**, plus power, Secure Boot, lockdown, and BMC-reset actions.

---

## 6. Behind the scenes: how a boot device is chosen and set

Boot configuration spans two components with separate ownership: **site-explorer** discovers candidate identities and initializes or enriches the durable desired target, while the observation-driven **machine-controller** applies and verifies that target for managed hosts. Site-explorer never replaces an existing operator-selected MAC. Managed-host selection and reapply APIs are declarative; raw `nico-admin-cli redfish …` commands remain direct. Explored-endpoint actions are also direct for unowned and DPU-owned endpoints. `MachineSetup` has one compatibility fallback: if a confirmed or predicted host has no resolvable target, it can still apply untargeted BIOS setup directly.

### Simplified boot-order flow

A host moves through these states (see the [Managed Host State Diagrams](../architecture/state_machines/managedhost.md) for the full picture):

This diagram intentionally summarizes the success and release paths; the canonical state-machine documentation linked above and the narrative below cover retry, failure, and persisted-resume behavior.

```text
Created → DpuDiscoveringState → HostInit → Validation → Ready
                (DPU hosts)        │
                                   ├─ EnableIpmiOverLan
                                   ├─ WaitingForPlatformConfiguration  (configure BIOS)
                                   ├─ WaitingForBiosJob                (Dell BIOS job)
                                   ├─ PollingBiosSetup                 (verify BIOS)
                                   ├─ SetBootOrder                     (set boot order)
                                   ├─ … (UEFI lockdown, measuring)
                                   └─ Discovered

Ready ── pending desired version ──→ BootConfiguring ── verified ──→ Ready
Ready ── allocation ──→ Assigned ── release ──→ Ready
                         └─ read-only drift observation; repair waits for release
```

Site-explorer creates the host in `Created` (DPUs, if any, in `DpuDiscoveringState`), records the boot **predictions**, and initializes the desired target as soon as selection is unambiguous. The machine-controller then drives `HostInit`, where it configures BIOS and sets the boot order. If final verification cannot be recorded there, the desired version remains pending and `Ready` enters the persisted `BootConfiguring` path before the host can be allocated.

### Resolving and persisting the boot interface

When no desired target exists, site-explorer initializes one with this precedence:

1. An explicitly declared primary prediction.
2. The host's effective owned interface (`primary_interface`, otherwise the lowest-MAC non-underlay row).
3. The sole non-underlay prediction when owned rows yield no candidate.

After initialization, site-explorer may add a newly discovered Redfish interface ID for the same MAC, but it does not select a different MAC. Operator changes through `SetPrimaryInterface` update the primary row and exact desired target together in one database transaction.

During `HostInit`, the shared boot driver resolves the current owned row or pre-first-lease prediction. During `BootConfiguring`, it instead captures the persisted desired target and version and keeps that exact input across restarts and in-flight vendor jobs. A missing lifecycle target is classified as:

- **AwaitingNic** — a host with no managed DPUs (zero-DPU, `ignore`, or `nic`) whose boot NIC hasn't appeared yet; wait.
- **Missing** — a host with managed DPUs has neither an owned primary interface nor a usable prediction; a fault to investigate. This includes a managed-DPU host declared to boot from an integrated HostInband NIC if that declaration did not produce a prediction.

> **Key timing.** Managed-DPU hosts usually acquire an owned primary host-facing row while the DPU is attached. A zero-DPU, NIC-mode, or declared integrated NIC can remain prediction-only until its first DHCP lease. When DHCP promotes that prediction to an owned row, the machine-scoped desired target remains stable across the handoff.

### Applying the boot order

- `configure_host_bios` (at `WaitingForPlatformConfiguration`) calls Redfish `machine_setup` with the resolved boot interface; on Dell this schedules a BIOS job (`WaitingForBiosJob`).
- `PollingBiosSetup` verifies the BIOS settings took.
- During `HostInit`, `SetBootOrder` targets the owned/predicted resolution via Redfish. In the default managed-DPU topology that is the primary DPU host-PF; for a declared integrated primary or a host using `ignore` or `nic`, it is the resolved HostInband interface. During `BootConfiguring`, the same stage uses the captured persisted desired target instead. A "no DPU" response from the BMC is expected and treated as success only for a host with no managed DPUs.
- An unassigned `Ready` host with an unverified desired version enters `BootConfiguring`. An ordinary host starts with a read-only Redfish inspection; a Supermicro host first disables lockdown and restarts because its locked boot-order view can be stale. The controller then reuses the shared BIOS/job/boot-order driver for only the work required, restores the configured lockdown policy, and records the exact desired version verified only after a final matching read. Persisted substates make the work restart-safe; a terminal failure stays visible and keeps the host out of allocation until maintenance or a fresh desired generation takes control.
- Every successful verification records `verified_version` and `observed_at`. A verified target becomes eligible for another read after `machine_state_controller.boot_interface_observation_interval` (default `10m`). Idle `Ready` and `Assigned` iterations inspect Redfish without mutating it once every managed DPU also has a current network observation. A successful match advances `observed_at`; a read failure leaves it unchanged so the next controller sweep retries. A mismatch opens a new pending generation: `Ready` repairs it, while `Assigned` defers disruptive work until the host is released.
- Managed-host selection and reapply paths update database state (primary and desired as applicable) and wake machine-controller; machine-controller decides whether BIOS work, boot-order work, or a reboot is required. Raw Redfish commands, DPU-owned endpoint actions, unowned endpoints, and the untargeted `MachineSetup` fallback retain direct behavior.
- On a reprovision repair, `check_host_boot_config` also re-checks BIOS + boot order and only remediates if they drifted.

---

## 7. The boot-interface data model

The durable machine-scoped request and its reconciliation/verification metadata live in `machine_boot_interfaces`. Predicted, managed, explored, and retained records supply discovery identity and defaults around that request.

### 7.1 Desired and verified (`machine_boot_interfaces`)

Each confirmed or predicted host can have one row:

- `desired_mac_address` and optional `desired_interface_id` identify a complete `Pair` or a valid MAC-only target while the Redfish ID is not yet known.
- `desired_version` starts at initialization and advances when an operator changes or force-reconciles a target, site-explorer adds the same MAC's Redfish ID, or periodic observation reopens the same target after drift. Same-MAC enrichment preserves a current verification because it strengthens the identity without selecting another NIC.
- `verified_version` identifies the last desired generation treated as verified, and `observed_at` records when that verification was established. A newer pending desired generation leaves both fields on the prior verification until convergence.
- `assumed` distinguishes the rollout compatibility baseline used when an already-stable `Ready` or `Assigned` host is initialized without a Redfish read; the next successful Redfish check clears it.

Reconciliation state is derived rather than stored as another mutable enum: matching desired and verified versions are `Converged`; otherwise current `BootConfiguring` work is `Converging` or `Failed`, and all other cases are `Pending`. The schema migration does not backfill targets. Site-explorer incrementally initializes missing rows from current interface or prediction data, one machine transaction at a time, and preserves an existing operator choice.

### 7.2 Predicted (`predicted_machine_interfaces`)

Site-explorer mints a prediction per declared host NIC **before** the host's first DHCP lease. A prediction carries `machine_id`, `mac_address`, `network_segment_type`, the operator's declared `primary` intent, and the `boot_interface_id` (the Redfish `EthernetInterface.Id`, captured from the exploration report once available). Predictions are what the controller uses to configure boot pre-lease.

### 7.3 Managed (`machine_interfaces`) — promotion

When the host first DHCPs on a predicted NIC, `move_predicted_machine_interface_to_machine` **promotes** the prediction into an owned `machine_interfaces` row:

- The row is created (or an existing static-preallocation row is reused) and associated with the machine.
- `primary_interface` is set from the prediction's declared intent; if it's primary, any prior primary on the machine is demoted first (so exactly one primary survives).
- The `boot_interface_id` is resolved by precedence: **prediction > existing row value > retained** (see below).
- The prediction is deleted — the owned row is now authoritative.

The owned table is **Store B**: the authoritative source for candidate and primary-interface selection once a host is owned, kept current by per-exploration updates. The separate `machine_boot_interfaces` row remains the authoritative desired Redfish target.

### 7.4 Retained boot interfaces

The Redfish boot-interface id is the one fact a MAC cannot always rediscover after deletion (a DPU/NIC-mode flip can drop the MAC from BMC reports while the id stays stable; a re-ingested host needs to be targeted for boot before a fresh exploration). So:

- On **deletion** of a `machine_interfaces` row, its `boot_interface_id` is **upserted** into `retained_boot_interfaces` (keyed by MAC; newest wins).
- On **creation** of a new row, any retained id for that MAC is **consumed** and applied — provided it's within the configured `retained_boot_interface_window` (default: no expiry, i.e. retained forever; set a window to bound recycled-MAC reuse).

This preserves the Redfish interface ID for the same MAC across a force-delete / re-ingest gap. Expected Machine, prediction, and owned-interface selection still choose the MAC; the machine-scoped desired row and version are deleted with the machine.

### 7.5 Selection precedence

The persisted desired target is authoritative for desired-state and `Ready` convergence. `HostInit` and reprovision lifecycle paths still resolve owned/predicted selection. The following functions compute initial/default candidates without replacing an existing desired MAC:

| Function | Operates on | Precedence |
|---|---|---|
| `desired_boot_interface_update` | durable-target initialization/enrichment | complete desired pair stays; a MAC-only target may gain the same MAC's ID; otherwise declared-primary prediction → owned pick → unambiguous prediction → none |
| `pick_boot_interface` | owned `machine_interfaces` | declared primary → lowest-MAC non-underlay → none |
| `pick_boot_prediction` | predictions | declared primary → the sole non-underlay prediction → none |
| `select_host_primary_interface` and Site Explorer fallback | the explored report (**Store A**, the default before ownership) | declared primary → lowest UEFI PCI path when every matching DPU host interface in the Redfish report has one → stable Redfish serial ordering → none |

**Store A vs. Store B:** before a host is owned, Site Explorer records a boot default on the explored endpoint (`explored_endpoints.boot_interface_mac`/`_id`, using `select_host_primary_interface`). Once owned, `machine_interfaces` (Store B) supplies current candidates. A declared `primary` wins in **both** stores, while `machine_boot_interfaces` preserves the selected desired target across the ownership handoff.

---

## 8. Verifying and troubleshooting

**Check a host's boot interface:**

```bash
nico-admin-cli -a <api-url> managed-host show <machine-id>
```

The interfaces section shows each NIC's MAC, segment, and which one is `primary`. The web UI machine-detail page also has a **Desired Boot Interface** section with the persisted target, reconciliation state, versions, last observation, active controller generation, and selectable candidates.

To inspect the boot interface itself — every store (managed, predicted, explored, retained) side by side, the effective owned pick, desired-state reconciliation (`Pending`, `Converging`, `Converged`, or `Failed`), and a flag when current selection signals disagree — use `boot-interface show <machine-id>`. The disagreement check compares the effective owned pick, explored defaults, and declared-primary predictions; it excludes retained history and the desired target. `boot-interface candidates <machine-id>` narrows the view to candidate NICs and includes the combined current pick, falling back to an unambiguous prediction before the first lease. Both commands also show the desired selection source and decision time. When NICo cannot recover the historical selection, including a migrated row or a compatibility baseline for an existing Ready or Assigned host, the commands report `LegacyUnknown` without a decision time. Both are read-only ([Section 4](#4-admin-cli-and-grpc-reference)).

To request another controller pass without changing the selected target, use **Request reconciliation** in the web UI. The CLI equivalent is `boot-interface set <machine-id> <interface> --force-reconcile` only when that interface is already the intended managed primary; the command is still a `SetPrimaryInterface` operation and can otherwise move the primary/admin network identity.

**Common situations:**

| Symptom | Likely cause / action |
|---|---|
| `boot_interface_mac_mismatch` (pairing blocker) | The host's boot MAC doesn't match any discovered DPU's pf0 MAC. Expected for an integrated-NIC host — declare the integrated NIC `primary` (see [3.4](#34-boot-an-integrated-nic-while-keeping-the-dpus-managed)); otherwise check the exploration reports. See [Ingesting Hosts → pairing blockers](ingesting-hosts.md#common-blockers-during-host--dpu-pairing). |
| Host stuck waiting for a boot NIC | A host with no managed DPUs (zero-DPU, `ignore`, or `nic`) whose boot NIC hasn't leased yet (`AwaitingNic`). Confirm the NIC is cabled and DHCP-reachable on its HostInband segment. |
| `Missing boot interface` for a managed-DPU host | The host has neither an owned primary interface nor a usable prediction. For an integrated-NIC boot, confirm `interfaces` declares exactly one `host` interface with `primary: true` and that site-explorer created its HostInband prediction; otherwise investigate DPU pairing and promotion. |
| Reconciliation is `Pending` while the host is `Assigned` | This is non-disruptive by design. The controller may observe drift but does not change Redfish or reboot a tenant host; repair begins after release returns it to unassigned `Ready`. |
| Reconciliation is `Failed` | Read the failure and active generation in `boot-interface show` or the web UI. Correct the reported underlying cause, then request reconciliation again for the same interface or select a new target. |
| `observed_at` is old | First compare `desired_version` and `verified_version`: a pending target retains the earlier generation's timestamp and is not eligible for periodic observation. For a verified target, a failed BMC read leaves the timestamp unchanged and retries on the next controller sweep. If Redfish is healthy, confirm the `boot_interface_observation_interval` has elapsed (default `10m`) and every managed DPU has a current network observation. Supermicro hosts with managed lockdown skip the non-disruptive periodic read because their locked boot-order view is stale. |
| Boot interface wrong after a DPU↔NIC-mode flip | For a managed host, use **Request reconciliation** (or a qualified `--force-reconcile` call as described above) to reapply its persisted target. **Restore Boot Interface** is also declarative when the explored endpoint belongs to a managed or predicted host; it applies Redfish directly only for an unowned or DPU-owned endpoint. Re-ingest if needed ([3.5](#35-flipping-a-dpu-to-nic-mode)). |
| `manual_power_cycle_required` (pairing blocker) after a queued NIC-mode flip | Site-explorer could not apply the queued mode change: The BMC refused both Redfish reset types (`PowerCycle` and `ACPowercycle`), or the DPUs still report the old mode after cycling. Power-cycle the host manually (BMC UI, or the admin CLI's `redfish ac-power-cycle`); the next exploration pass verifies the mode and clears the blocker. A host can also sit here briefly mid-flip; repeat cycles are rate-limited. |
| Boot interface disabled or no longer the boot device after a DPU replacement (notably on Dell) | The replacement DPU came up in InfiniBand (VPI) link type. NICo self-heals this: DPU cloud-init normalizes the ports to Ethernet and reboots before management-network setup runs, and DPU reprovision then repairs the host BIOS/boot-order configuration automatically — no manual action is needed. If it persists, confirm the DPU ran the normalization (`/var/log/forge/link-type.log` on the DPU) and re-ingest. |
| DPU mode "unknown" (`dpu_nic_mode_unknown`) | DPU BMC firmware too old to report mode. Install a fresh DPU OS — see [Ingesting Hosts](ingesting-hosts.md#dpu-related-issues-installing-a-fresh-dpu-os). |

For ingestion/pairing diagnostics generally, see [Ingesting Hosts → Troubleshooting](ingesting-hosts.md#troubleshooting-host-and-dpu-ingestion-issues).

---

## Related pages

- [Ingesting Hosts](ingesting-hosts.md) — the end-to-end ingest flow and the base `expected_machines.json`.
- [IP and Network Configuration](ip-and-network-configuration.md) — network segments, DHCP relay/`giaddr` → segment matching.
- [Force Delete](../playbooks/force_delete.md) — the re-ingest procedure used when flipping DPU modes.
- [Managed Host State Diagrams](../architecture/state_machines/managedhost.md) — the full host state machine.
- [DPU Lifecycle Management](../dpu-management/dpu-lifecycle-management.md) — DPU OS install, firmware, health, reprovision.
