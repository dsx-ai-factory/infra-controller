# Ingesting Hosts (REST API)

Provider-side hardware onboarding for NICo using the REST API and `nicocli`. This is the Day 0 prequel to the Typical API Call Flows in the REST API Reference -- it covers how physical machines become discoverable so that subsequent calls like Retrieve All Machines return real hardware to allocate and provision.

## Before You Start

Make sure the following are in place before you begin:

1. NICo is deployed and the REST API service is reachable at a known URL.
1. You have `nicocli` installed (`make nico-cli` from the infra-controller repo) and a working config under `~/.nico/`. For setup, authentication, and config conventions, see the [Quick Start Guide](../getting-started/quick-start.md) and the nicocli reference guide.
1. You hold the `PROVIDER_ADMIN` role in the org you are operating in. Tenant Admins with `targetedInstanceCreation` capability can also register Expected Machines, but the canonical path is provider-side.
1. DHCP requests from all managed host BMC networks have been forwarded to the NICo DHCP service.
1. For every host you plan to register, you have:
   - The MAC address of the host BMC
   - The chassis serial number
   - The host BMC factory default username and password

Verify connectivity:

```bash
nicocli site list
nicocli user get
```

`nicocli site list` returns the Sites the calling org has access to. Pick the Site UUID for the data center you are onboarding hardware into -- you will pass it as `--site-id` in every Expected Machine create call.

## Registering Expected Machines

An Expected Machine pre-registers a physical machine so NICo can authenticate to it on discovery and accept it for ingestion. Each Expected Machine carries the factory default BMC credentials NICo uses for first contact, plus identifying information (chassis serial, optional rack/SKU metadata).

The Expected Machine endpoints are scoped per-org per-site. All requests require `PROVIDER_ADMIN` (or `TENANT_ADMIN` with `targetedInstanceCreation`).

### Single Machine

Prompt for the BMC password and send the request body over stdin so the secret
does not appear in shell history or the process list:

```bash
read -r -s -p 'Factory-default BMC password: ' NICO_PASSWORD
printf '\n'
printf '%s' "$NICO_PASSWORD" |
  jq -Rs \
    --arg siteId '<site-uuid>' \
    --arg bmcMacAddress '<mac>' \
    --arg chassisSerialNumber '<chassis-serial>' \
    --arg defaultBmcUsername '<bmc-user>' \
    '{
      siteId: $siteId,
      bmcMacAddress: $bmcMacAddress,
      chassisSerialNumber: $chassisSerialNumber,
      defaultBmcUsername: $defaultBmcUsername,
      defaultBmcPassword: .
    }' |
  nicocli expected-machine create --data-file -
unset NICO_PASSWORD
```

Required request fields are `siteId`, `bmcMacAddress`, and
`chassisSerialNumber`. The BMC credentials are optional on the REST schema but
required in practice -- without them NICo cannot authenticate to the BMC for
discovery.

Optional flags add metadata or pre-allocate resources:

| Flag | Purpose |
| ---- | ------- |
| `--bmc-ip-address` | Pre-allocate a reserved BMC IP (IPv4 or IPv6) instead of letting DHCP assign one |
| `--rack-id` | Associate with a rack identifier |
| `--sku-id` | Associate with a SKU |
| `--manufacturer`, `--model`, `--firmware-version`, `--name`, `--description` | Free-text hardware metadata |
| `--slot-id`, `--tray-idx`, `--host-id` | Physical placement within a rack/tray |

You can also pass additional fields in the stdin JSON object. Keep the password
on stdin rather than writing it into the command:

```bash
read -r -s -p 'Factory-default BMC password: ' NICO_PASSWORD
printf '\n'
printf '%s' "$NICO_PASSWORD" |
  jq -Rs \
    --arg siteId '<site-uuid>' \
    --arg bmcMacAddress '<mac>' \
    --arg chassisSerialNumber '<chassis-serial>' \
    --arg defaultBmcUsername '<bmc-user>' \
    '{
      siteId: $siteId,
      bmcMacAddress: $bmcMacAddress,
      defaultBmcUsername: $defaultBmcUsername,
      defaultBmcPassword: .,
      chassisSerialNumber: $chassisSerialNumber,
      hostLifecycleProfile: {disableLockdown: true},
      fallbackDPUSerialNumbers: ["<dpu-serial-1>", "<dpu-serial-2>"],
      labels: {environment: "production", rack: "A1"}
    }' |
  nicocli expected-machine create --data-file -
unset NICO_PASSWORD
```

`fallbackDPUSerialNumbers` is JSON-only (no flag form) and is needed for DGX-H100 or other machines where the NetworkAdapter serial number is not available in the host Redfish.

`hostLifecycleProfile` is also JSON-only. Set `disableLockdown` to `true` only when the host must remain unlocked during lifecycle management. When the profile is omitted or `disableLockdown` is `false`, NICo applies UEFI lockdown during `HostInit`.

### Batch (Recommended for Full-Rack Onboarding)

For multiple machines, prefer `batch-create`. The endpoint accepts up to 100 Expected Machines per request and validates the whole batch atomically:

```bash
nicocli expected-machine batch-create --data-file expected-machines.json
```

Where `expected-machines.json` is a JSON array of `ExpectedMachineCreateRequest` objects:

```json
[
  {
    "siteId": "<site-uuid>",
    "bmcMacAddress": "<mac-1>",
    "defaultBmcUsername": "<bmc-user>",
    "defaultBmcPassword": "<bmc-password-1>",
    "chassisSerialNumber": "<chassis-serial-1>",
    "hostLifecycleProfile": {
      "disableLockdown": true
    },
    "rackId": "rack-01"
  },
  {
    "siteId": "<site-uuid>",
    "bmcMacAddress": "<mac-2>",
    "defaultBmcUsername": "<bmc-user>",
    "defaultBmcPassword": "<bmc-password-2>",
    "chassisSerialNumber": "<chassis-serial-2>",
    "rackId": "rack-01"
  }
]
```

Two constraints apply to every batch request, both enforced at the API gateway:

- **Maximum 100 machines per request.** For sites with more than 100 machines, send multiple `batch-create` calls.
- **All machines in one request must share the same `siteId`.** You cannot mix sites within a single batch.

## What Happens After Approval: Ingestion to Ready

Once Expected Machines are registered and the trust policy is in place, NICo's Site Explorer automatically discovers and ingests each machine. No further operator action is required under normal circumstances.

The high-level flow:

1. **DHCP discovery**: the host BMC sends a DHCP request; NICo assigns an IP and Site Explorer probes the BMC over Redfish using the factory default credentials from the Expected Machine, then rotates the BMC password to the site-wide credential. See [Redfish Workflow](../architecture/redfish_workflow.md).
1. **Preingestion**: NICo runs a preingestion state machine against each discovered BMC endpoint (host and DPU). It checks BMC clock drift against site time, resetting the BMC if needed. For host endpoints, firmware components are upgraded to the minimum version required for ingestion.
1. **DPU-host pairing**: Site Explorer correlates host and DPU serial numbers to form matched pairs. Once DPUs are validated and paired, the `ManagedHost` object is created and the state machine starts.
1. **`DpuDiscoveringState` / `DPUInit`**: NICo configures Secure Boot on the DPU, installs the DPU OS (BFB image), and power-cycles the host to apply the new DPU configuration.
1. **`HostInit`**: NICo configures BIOS, sets the host boot order, optionally collects TPM attestation measurements, waits for hardware discovery via the `scout` agent, and applies UEFI lockdown unless `hostLifecycleProfile.disableLockdown` is `true`. When `scout` reports back, NICo replaces the temporary predicted host ID (prefix `fm100p`) with a stable host ID (prefix `fm100h`) derived from the host's DMI serial data or TPM certificate.
1. **`BomValidating` / `Validation`**: NICo validates discovered hardware against the expected SKU. If hardware validation is enabled, the host is rebooted and tested before proceeding.
1. **`Ready`**: the host transitions through `HostInit/Discovered` and enters the available pool, ready for an instance to be assigned.

For the full DPU lifecycle, see [DPU Lifecycle Management](../dpu-management/dpu-lifecycle-management.md). For the complete state transitions, see [Managed Host State Diagrams](../architecture/state_machines/managedhost.md).

## Verifying Ingestion

Once machines reach `Ready`, they show up in the Machine REST endpoint. List all machines on a site:

```bash
nicocli machine list --output table
```

Inspect a single machine:

```bash
nicocli machine get <machine-id>
```

Include hardware metadata (CPU, memory, interfaces, etc.):

```bash
nicocli machine get <machine-id> --include-metadata
```

A `Ready` machine has `status: Ready` and `isUsableByTenant: true`. Once at least one machine is `Ready`, you can continue with the rest of the Provider or Service Account flow.

## What's Next

With machines ingested and `Ready`, follow the relevant API flow in the REST API Getting Started reference:

- **Service Account**: create Network Allocations against each Site IP Block,
  create a VPC, wait until it is `Ready`, and create the matching tenant
  network resource. `FNN` VPCs use VPC Prefixes; `ETHERNET_VIRTUALIZER` VPCs
  use IPv4 Subnets from a `Ready` tenant-allocated IPv4 IP Block at the VPC's
  Site; `FLAT` VPCs use neither resource. For a `FLAT` VPC, set `autoNetwork`
  to `true` and omit `interfaces` when creating the Instance. Then create an
  Operating System and an Instance.
- **Provider**: create Tenant Accounts, create Instance Types, associate machines with Instance Types, create Compute and Network Allocations for tenants.

Both flows assume hardware ingestion is complete -- which this page covers.

## Troubleshooting

### Inspecting Machine State

When a machine is not being created or is stuck in a pre-`Ready` state, start with the Machine REST endpoint:

```bash
nicocli machine list --output table
nicocli machine get <machine-id>
```

For deeper investigation, `nico-api` logs filtered by the host BMC IP or DPU BMC IP are the fastest way to understand where ingestion or pairing is failing.

### Endpoint Exploration Errors

Before pairing can occur, Site Explorer must successfully explore each BMC endpoint. Exploration failures are logged in `nico-api` and the NICo Grafana dashboard. Common error types:

| Error type | Likely cause |
|---|---|
| `ConnectionTimeout` | BMC unreachable on the OOB network; check cabling and DHCP routing |
| `ConnectionRefused` | No Redfish API exposed at the target IP; the DPU admin IP is often mistakenly probed here |
| `Unauthorized` / `AvoidLockout` | BMC credentials do not match the Expected Machine entry or site vault; see [Adding New Machines: BMC Password Requirements](../playbooks/stuck_objects/adding_new_machines.md) |
| `MissingCredentials` | Credentials not yet available from any configured credential source; check that site-wide BMC credentials are configured |
| `UnsupportedVendor` | BMC vendor is not supported by this version of NICo |
| `RedfishError` | Unexpected Redfish response; check BMC firmware version and `nico-api` logs for the full response body |
| `InvalidDpuRedfishBiosResponse` | DPU BIOS endpoint returned an unexpected response; the DPU may need a fresh OS install |

For a complete reference of all Redfish endpoints and required response fields, see [Redfish Endpoints Reference](../architecture/redfish/endpoints_reference.md).

### Common Blockers During Host + DPU Pairing

The following are the conditions in which Site Explorer cannot complete pairing and logs a `host_dpu_pairing_blockers_count` metric. Each requires operator investigation.

| Metric label | Description | Action |
|---|---|---|
| `dpu_nic_mode_unknown` | DPU mode cannot be determined; DPU BMC firmware is likely too old | Install a fresh DPU OS (which also upgrades firmware) |
| `dpu_pf0_mac_missing` | DPU is in DPU mode but its pf0 MAC address is not retrievable | Install a fresh DPU OS |
| `manual_power_cycle_required` | DPU mode was changed but the host vendor does not support automated power cycling | Manually power-cycle the host at the data center level |
| `host_system_report_missing` | Host BMC Redfish returned no valid system report; likely a BMC firmware issue or transient error | Check `nico-api` logs for the host BMC IP |
| `no_dpu_reported_by_host` | Host BMC reports no BlueField PCIe devices | Check DPU seating and host BMC firmware version |
| `boot_interface_mac_mismatch` | Host boot MAC does not match the pf0 MAC of any discovered DPU | Check exploration reports and `nico-api` logs for both host and DPU BMC IPs |
| `viking_cpld_version_issue` | NVIDIA Viking (DGX): `CPLDMB_0` firmware below minimum required version (`0.2.1.9`) | Contact the data center team for a full DC power cycle |

For more DPU-specific troubleshooting (Secure Boot configuration, BMC password resets, firmware version checks), see [Adding New Machines to an Existing Site](../playbooks/stuck_objects/adding_new_machines.md).

## Managing the Expected Machines Table

### Listing and Filtering

```bash
nicocli expected-machine list --output table
nicocli expected-machine list --site-id <site-uuid> --output table
nicocli expected-machine list --include-relation Site --output table
```

Pagination is on by default (`--page-number`, `--page-size`); use `--all` to fetch every page. Sort order is controlled by `--order-by` (see `--help` for the supported enum values).

### Single-Entry Operations

```bash
nicocli expected-machine get <expected-machine-id>
nicocli expected-machine update <expected-machine-id> --rack-id rack-02
nicocli expected-machine delete <expected-machine-id>
```

`update` accepts the same flags as `create` (all optional on update). For complex updates, use `--data-file` with the full `ExpectedMachineUpdateRequest` JSON.

### Batch Update

```bash
nicocli expected-machine batch-update --data-file updates.json
```

Where `updates.json` is a JSON array of `ExpectedMachineUpdateRequest` objects. Each entry must include the `id` field identifying which Expected Machine to update; other fields are merged onto the existing record.

### Export

To dump the current table as JSON:

```bash
nicocli expected-machine list --all --output json
```

Suitable for backup or import-into-another-site workflows.
