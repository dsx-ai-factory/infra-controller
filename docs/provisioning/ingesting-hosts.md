# Ingesting Hosts

Once you have NVIDIA Infra Controller (NICo) up and running, you can begin ingesting machines.

Hosts boot the discovery OS (Scout) as part of this flow. To run site-specific
cloud-init configuration during discovery boots, see [Discovery OS cloud-init
(Scout)](discovery-os-cloud-init.md).

The preferred operator workflow uses the REST API and `nicocli`. Follow [Ingesting Hosts (REST API)](ingesting-hosts-rest-api.md) for credential setup, Expected Machine registration, ingestion verification, and table maintenance. The direct Core workflow below remains available for operations that do not yet have REST parity; see [Site Setup API Parity](site-setup-api-parity.md) for the current status and tracked gaps.

## Prerequisites

Ensure you have the following prerequisites met before ingesting machines:

1. You have `nicocli` installed and configured for the target REST API. See the [Quick Start Guide](../getting-started/quick-start.md).
1. For the remaining REST parity gaps, you have `nico-admin-cli` and direct access to the NICo site. See the next section for details.
1. The NICo API service is running at IP address `NICo_API_EXTERNAL`. It is recommended that you add this IP address to your trusted list.
1. DHCP requests from all managed host IPMI networks have been forwarded to the NICo service running at IP address `NICo_DHCP_EXTERNAL`.
1. You have the following information for all hosts that need to be ingested:

   - The MAC address of the host BMC
   - The chassis serial number
   - The host BMC username (typically this is the factory default username)
   - The host BMC password (typically this is the factory default password)

<Steps toc={true}>

## Get client key and certificate needed for nico-admin-cli

These can be generated from site vault. Follow these steps to generate them.NICO_LB_IP

### Prerequisites

1. Check `additional_issuer_cns` (one-time per cluster).

   ```bash
   kubectl get configmap -n nico-system nico-api-config-files -o yaml | grep -i "additional_issuer_cns"
   ```

   Expected: `additional_issuer_cns = ["site-root"]`

   If it's empty, edit the configmap and set it, then restart:

   ```bash
   kubectl -n nico-system edit configmap nico-api-config-files
   # under [auth.trust]: additional_issuer_cns = ["site-root"]

   kubectl rollout restart deployment/nico-api -n nico-system
   ```

1. Get the CLI binary - You can skip this step if you already have the `nico-admin-cli` binary.

   ```bash
   POD=$(kubectl -n nico-system get pods -l app.kubernetes.io/name=nico-api -o jsonpath='{.items[0].metadata.name}')
   kubectl -n nico-system cp   "${POD}:/opt/carbide/nico-admin-cli"   /usr/local/bin/nico-admin-cli
   chmod +x /usr/local/bin/nico-admin-cli
   # verify that it is working
   nico-admin-cli
   ```

1. Issue a client cert from Vault.

   ```bash
   VAULT_TOKEN=$(kubectl -n vault get secret vaultroottoken -o jsonpath='{.data.token}' | base64 -d)
   kubectl -n vault exec vault-0 -- env VAULT_SKIP_VERIFY=true VAULT_TOKEN="$VAULT_TOKEN" \
     vault write -format=json nicoca/issue/nico-cluster \
     common_name="<FQDN for nico-api-endpoint>" \
     ttl=720h > /tmp/issued.json
   ```

   Replace `<FQDN for nico-api-endpoint>` appropriately which usually is `api-<ENVIRONMENT_NAME>.<SITE_DOMAIN_NAME>`

1. Extract PEM files.

    ```bash
    cat /tmp/issued.json | jq -r '.data.private_key' > /path/to/client.key
    cat /tmp/issued.json | jq -r '.data.certificate' > /path/to/client.crt
    cat /tmp/issued.json | jq -r '.data.issuing_ca' >  /path/to/ca.crt
    ```

   Set the variables below to the target API and certificate paths. You can then run admin CLI commands as follows:

   ```bash
   NICO_API_URL='https://api.example.com'
   NICO_ROOT_CA_PATH='/path/to/ca.crt'
   NICO_CLIENT_CERT_PATH='/path/to/client.crt'
   NICO_CLIENT_KEY_PATH='/path/to/client.key'

   nico-admin-cli \
     -a "$NICO_API_URL" \
     --root-ca-path "$NICO_ROOT_CA_PATH" \
     --client-cert-path "$NICO_CLIENT_CERT_PATH" \
     --client-key-path "$NICO_CLIENT_KEY_PATH" \
     --help
   ```

   Alternatively, to shorten the command line, you can create a file named `nico_api_cli.json` in folder `$HOME/.config` and add the following content:

   ```json
   {
     "api_url": "https://api-<ENVIRONMENT_NAME>.<SITE_DOMAIN_NAME>:443",
     "root_ca_path": "/path/to/ca.crt",
     "client_cert_path": "/path/to/client.crt",
     "client_key_path": "/path/to/client.key"
   }
   ```

1. Add an `/etc/hosts` entry.

   If you have trouble resolving `api-<ENVIRONMENT_NAME>.<SITE_DOMAIN_NAME>` you have to map it to the LoadBalancer IP:

   ```bash
   NICO_LB_IP=$(kubectl -n nico-system get svc nico-api-external \
     -o jsonpath='{.status.loadBalancer.ingress[0].ip}')

   grep -q "api-<ENVIRONMENT_NAME>.<SITE_DOMAIN_NAME>" /etc/hosts || \
     echo "$NICO_LB_IP api-<ENVIRONMENT_NAME>.<SITE_DOMAIN_NAME>" | sudo tee -a /etc/hosts
   ```

## Update Site

NICo requires knowledge of the current and desired BMC and UEFI credentials for hosts and DPUs. NICo will reset current crendtials to the desired credentials on the BMC and UEFI when ingesting a host. You can use these credentials when accessing the host or DPU BMC yourself, and NICo will use these credentials for its automated processes.

The required credentials include the following:

- Host BMC Credential
- DPU BMC Credential
- Host UEFI password
- DPU UEFI password

<Note>
The following commands use the `<api-url>` placeholder, which is typically the following:

```bash
https://api-<ENVIRONMENT_NAME>.<SITE_DOMAIN_NAME> \
  --nico-root-ca-path <NICO_ROOT_CA_PATH> \
  --client-cert-path <CLIENT_CERT_PATH> \
  --client-key-path <CLIENT_KEY_PATH>
```

</Note>

### Store Host and DPU BMC Password

Run this command to store the desired Host and DPU BMC password:

```bash
read -r -s -p 'Site-wide BMC password: ' NICO_PASSWORD
printf '\n'
printf '%s' "$NICO_PASSWORD" |
  jq -Rs --arg siteId '<site-uuid>' \
    '{siteId: $siteId, kind: "SiteWideRoot", password: .}' |
  nicocli bmc-credential create --data-file -
unset NICO_PASSWORD
```

### Store Host and DPU UEFI Passwords

Run this command to store the desired host UEFI password:

```bash
read -r -s -p 'Host UEFI password: ' NICO_PASSWORD
printf '\n'
printf '%s' "$NICO_PASSWORD" |
  jq -Rs --arg siteId '<site-uuid>' \
    '{siteId: $siteId, kind: "Host", password: .}' |
  nicocli uefi-credential create --data-file -
unset NICO_PASSWORD
```

Run this command to store the desired DPU UEFI password:

```bash
read -r -s -p 'DPU UEFI password: ' NICO_PASSWORD
printf '\n'
printf '%s' "$NICO_PASSWORD" |
  jq -Rs --arg siteId '<site-uuid>' \
    '{siteId: $siteId, kind: "DPU", password: .}' |
  nicocli uefi-credential create --data-file -
unset NICO_PASSWORD
```

## Add Expected Machines Table

NICo needs to know the factory default credentials for each BMC, which is expressed as a JSON table of "Expected Machines". The serial number is used to verify the BMC MAC matches the actual serial number of the chassis.

Register a single Expected Machine with `nicocli`:

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

For more than one machine, prepare the JSON array documented in [Ingesting Hosts (REST API)](ingesting-hosts-rest-api.md#batch-recommended-for-full-rack-onboarding), then run:

```bash
nicocli expected-machine batch-create --data-file expected-machines.json
```

Only registered Expected Machines will be ingested.

For optional REST fields and batch JSON examples, use [Ingesting Hosts (REST API)](ingesting-hosts-rest-api.md#registering-expected-machines).

### Per-Host Fields With the Admin CLI

The admin CLI's `expected-machine` command (`em`) provides the complete set of
per-host fields and compatibility controls. It reads a snake_case
`expected_machines.json` file and applies the whole table at once:

- **`dpu_policy`** (`"manage"` | `"nic"` | `"ignore"`): Per-host policy for managing DPU hardware. `nic` and `ignore` override the site policy; `manage` or an omitted field inherits the site-wide policy, which defaults to `manage`. The previous `"use_as_nic"` value is still accepted, as are manifests using the `dpu_mode` key with `"dpu_mode"`, `"nic_mode"`, or `"no_dpu"` values.
- **`dpf_enabled`** (bool, default `true`): Enable or disable DPF provisioning for this host. When omitted or `true`, DPF is the provisioning path (requires `[dpf].enabled = true` in the site config — see [DPF setup](../manuals/dpf.md)). Set to `false` to keep a host on the deprecated iPXE path.
- **`bmc_retain_credentials`** (bool): Skip BMC password rotation.
- **`default_pause_ingestion_and_poweron`** (bool): Pause ingestion and power-on for this host.
- **`interfaces`** (array): Configure Host, DPU OS, DPU BMC, and Host BMC
  interface roles and their Dynamic, Fixed, or Retained IP allocation policies.
  See [Configure Expected Machine Interfaces](expected-machine-interfaces.md).
- **`bmc_ip_address`** (string, optional): Compatibility top-level Host BMC
  address. With the top-level `Auto` policy, its presence infers Fixed
  allocation.
- **`bmc_ip_allocation`** (`"Unspecified"` | `"Auto"` | `"Dynamic"` |
  `"Fixed"` | `"Retained"`, default `"Auto"`): Compatibility Host BMC policy
  in the whole-table JSON consumed by `em replace-all`. For the top-level
  compatibility form, `Auto` resolves to `Fixed` when `bmc_ip_address` is
  present and `Retained` otherwise; `Unspecified` resets to `Auto`. Explicit
  top-level address and policy values override the nested `host_bmc` values.
  `Dynamic` and `Retained` cannot be combined with
  `bmc_ip_address`; use `Fixed` or `Auto` with an address. The
  corresponding `em add` and `em patch` flag values are lowercase. See
  [Host BMC compatibility fields](ip-and-network-configuration.md#host-bmc-compatibility-fields)
  for field interactions and the supported shared HostInband topology.
- **`host_lifecycle_profile.disable_lockdown`** (bool, default `false`): When `true`, the state machine does not lock down the host during lifecycle management, which suits automation workflows that keep lockdown disabled.

The previous `host_nics` manifest key and `--host_nics` CLI option remain
aliases for `interfaces` and `--interfaces`. Existing manifests do not need
conversion. Do not set both names within the same Expected Machine entry.

Each manifest entry combines the required BMC credentials with any of these fields:

```json
{
  "expected_machines": [
    {
      "bmc_mac_address": "C4:5A:B1:C8:38:0D",
      "bmc_username": "root",
      "bmc_password": "<bmc-password>",
      "chassis_serial_number": "SERIAL-1",
      "dpu_policy": "nic"
    }
  ]
}
```

Apply the manifest with the admin CLI:

```bash
nico-admin-cli -a <api-url> em replace-all --filename expected_machines.json
```

Because `em replace-all` sets the entire table, include every host in the file. To change one entry in place, use `nico-admin-cli em patch` (for example, `nico-admin-cli em patch --bmc-mac-address <bmc-mac> --dpu-policy nic`).

## Approve all Machines for Ingestion

NICo uses Measured Boot using the on-host Trusted Platform Module (TPM) v2.0 to enforce cryptographic identity of the host hardware and firmware. The following command configures NICo to approve all pending machines based on PCR Registers 0, 3, 5, and 6:

```bash
nico-admin-cli -a <api-url> att mb site trusted-machine approve \* persist --pcr-registers="0,3,5,6"
```

</Steps>

## What Happens After Approval: Ingestion to Ready

Once machines are approved, NICo's Site Explorer begins automatically ingesting them. No further operator action is required under normal circumstances.

The high-level flow is:

1. **DHCP discovery**: the host BMC sends a DHCP request; NICo assigns an IP and Site Explorer probes the BMC over Redfish to collect a full inventory. Site Explorer authenticates using the factory default credentials from the expected machines table, then rotates the BMC password to the site-wide credential. See [Redfish Workflow](../architecture/redfish_workflow.md) for details.
1. **Preingestion**: before pairing, NICo runs a preingestion state machine against each discovered BMC endpoint (both host and DPU). It checks that the BMC clock is within an acceptable drift of the site time, resetting the BMC if not. For host endpoints, firmware components are upgraded if they are below the minimum version required for ingestion.
1. **DPU-host pairing**: Site Explorer correlates host and DPU serial numbers to form matched pairs. Once all DPUs are validated and matched, the `ManagedHost` object is created and the state machine starts.
1. **`DpuDiscoveringState` / `DPUInit`**: NICo configures Secure Boot on the DPU, installs the DPU OS (BFB image), and power-cycles the host to apply the new DPU configuration.
1. **`HostInit`**: NICo configures BIOS, sets the host boot order, optionally collects TPM attestation measurements, waits for hardware discovery via the `scout` agent, and applies UEFI lockdown. When the `scout` agent reports back, NICo replaces the temporary predicted host ID (prefix `fm100p`) with a stable host ID (prefix `fm100h`) derived from the host's own DMI serial data or TPM certificate.
1. **`BomValidating` / `Validation`**: NICo validates the discovered hardware against the expected SKU. If hardware validation is enabled, the host is rebooted and tested before proceeding.
1. **`Ready`**: the host transitions through `HostInit/Discovered` and enters the available pool, ready for an instance to be assigned to it.

For the full DPU lifecycle — OS installation, firmware upgrades, health monitoring, and reprovisioning — see [DPU Lifecycle Management](../dpu-management/dpu-lifecycle-management.md). For the complete state transitions, including substates, retry logic, and reprovision paths, see the [Managed Host State Diagrams](../architecture/state_machines/managedhost.md).

---

## Troubleshooting: Host and DPU Ingestion Issues

When a machine is not being created or is stuck in a pre-`Ready` state, `nico-api` logs are the primary investigation tool. Filtering logs by the host BMC IP or DPU BMC IP is often the fastest way to understand where ingestion or pairing is failing.

You can check the current detailed state of any managed host using:

```bash
nicocli machine list --output table
nicocli machine get <machine-id>
```

For a full guide on diagnosing stuck objects, including how to use the NICo Grafana dashboard and how to read state handler error logs, see [Stuck Objects Runbook](../playbooks/stuck_objects/stuck_objects.md).

### Endpoint Exploration Errors

Before pairing can occur, Site Explorer must successfully explore each BMC endpoint. Exploration failures are logged and surfaced in `nico-api` logs and the NICo Grafana dashboard. Common error types:

| Error type | Likely cause |
|---|---|
| `ConnectionTimeout` | BMC unreachable on the OOB network; check cabling and DHCP routing |
| `ConnectionRefused` | No Redfish API exposed at the target IP; the DPU admin IP is often mistakenly probed here |
| `Unauthorized` / `AvoidLockout` | BMC credentials do not match the expected machines table or site vault; see [Adding New Machines: BMC Password Requirements](../playbooks/stuck_objects/adding_new_machines.md) |
| `MissingCredentials` | Credentials not yet available from any configured credential source; check that site-wide BMC credentials are configured |
| `UnsupportedVendor` | BMC vendor is not supported by this version of NICo |
| `RedfishError` | Unexpected Redfish response; check BMC firmware version and `nico-api` logs for the full response body |
| `InvalidDpuRedfishBiosResponse` | DPU BIOS endpoint returned an unexpected response; the DPU may need a fresh OS install |

For a complete reference of all Redfish endpoints and required response fields, see [Redfish Endpoints Reference](../architecture/redfish/endpoints_reference.md).

### Common Blockers During Host + DPU Pairing

The following are the conditions in which Site Explorer cannot complete pairing and logs a `host_dpu_pairing_blockers_count` metric. Each requires operator investigation.

| Metric label | Description | Action |
|---|---|---|
| `dpu_nic_mode_unknown` | DPU mode cannot be determined; DPU BMC firmware is likely too old. | Install a fresh DPU OS (which also upgrades firmware); see [Installing a Fresh DPU OS](#dpu-related-issues-installing-a-fresh-dpu-os) below |
| `dpu_pf0_mac_missing` | DPU is in DPU mode but its pf0 MAC address is not retrievable. | Install a fresh DPU OS; see [Installing a Fresh DPU OS](#dpu-related-issues-installing-a-fresh-dpu-os) below |
| `manual_power_cycle_required` | DPU mode was changed but the host vendor does not support automated power cycling. | Manually power-cycle the host at the data center level |
| `host_system_report_missing` | Host BMC Redfish returned no valid system report; likely a BMC firmware issue or transient error. | Check `nico-api` logs for the host BMC IP |
| `no_dpu_reported_by_host` | Host BMC reports no BlueField PCIe devices. | Check DPU seating and host BMC firmware version |
| `boot_interface_mac_mismatch` | Host boot MAC does not match the pf0 MAC of any discovered DPU. | Check exploration reports and `nico-api` logs for both the host and DPU BMC IPs |
| `viking_cpld_version_issue` | NVIDIA Viking (DGX): `CPLDMB_0` firmware below minimum required version (`0.2.1.9`). | Contact the data center team for a full DC power cycle |

### DPU-Related Issues: Installing a Fresh DPU OS

For DPU pairing failures, including `dpu_pf0_mac_missing` and cases where the DPU is in an unknown or corrupt state, a common fix is to install a vanilla pre-ingestion BFB image via rshim to return the DPU to a clean state. This runs as part of the preingestion state machine:

```bash
nico-admin-cli -a <api-url> site-explorer copy-bfb-to-dpu-rshim \
  --host-bmc-ip <host-bmc-ip> \
  <dpu-bmc-ip>
```

This command copies the NICo BFB image directly to the DPU via rshim (SSH to the DPU BMC) and triggers a DPU reboot to complete the installation. After the BFB is installed, NICo power-cycles the host automatically to apply the new DPU image.

<Note>
The `--host-bmc-ip` flag is required. NICo uses it to power-cycle the host after the BFB copy completes. Use `--pre-copy-powercycle` if the host needs to release rshim control to the DPU BMC before the copy can start.
</Note>

For additional DPU-specific troubleshooting including Secure Boot configuration, BMC password resets, and firmware version checks, see [Adding New Machines to an Existing Site](../playbooks/stuck_objects/adding_new_machines.md).

---

## Managing the Expected Machines Table

The expected machines table in the nico-api database holds the following fields per host:

- Chassis Serial Number
- BMC MAC Address
- BMC manufacturer's set login
- BMC manufacturer's set password
- DPU chassis serial number (only needed for DGX-H100 or other machines where the NetworkAdapter serial number is not available in the host Redfish)

### Individual operations

Use `nicocli` to operate on individual entries:

```bash
EXPECTED_MACHINE_ID='expected-machine-id'
nicocli expected-machine update \
  --description '<description>' \
  "$EXPECTED_MACHINE_ID"
nicocli expected-machine delete "$EXPECTED_MACHINE_ID"
```

To create another entry, use the password-safe stdin workflow in
[Add Expected Machines Table](#add-expected-machines-table).

### Bulk operations

Create or update entries from a JSON file:

```bash
nicocli expected-machine batch-create --data-file expected-machines.json
nicocli expected-machine batch-update --data-file expected-machine-updates.json
```

See [Ingesting Hosts (REST API)](ingesting-hosts-rest-api.md#batch-update) for the batch update JSON shape.

Delete an entry by ID:

```bash
nicocli expected-machine delete "$EXPECTED_MACHINE_ID"
```

### Export

Export the current table as JSON:

```bash
nicocli expected-machine list --all --output json
```
