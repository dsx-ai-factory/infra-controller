# Configure Firmware Versions <Badge intent="info">v2.1</Badge> <Badge intent="launch" minimal>New</Badge>

NICo needs a site-specific definition of acceptable firmware before it can
detect drift or choose an upgrade target. Host firmware and DPU NIC firmware
use different configuration models. Rack and component updates use a target
supplied by the request or selected by their backend.

The firmware catalog answers **what firmware NICo recognizes and should
install**. Start there: define host firmware through the
[Host Firmware Config API](api:PUT/v2/org/:org/nico/firmware-config/host)
or legacy metadata. Later sections cover DPU baselines and the site controls
that determine **when NICo looks for work** and **how much work it may start**.

## Host firmware catalog

Each host firmware definition is associated with a BMC vendor and host model.
The definition can come from the Host Firmware Config API, a legacy
`metadata.toml` file, or the static `host_models` configuration. NICo merges
these sources into one effective catalog.

During site exploration, Redfish reports the BMC vendor and host model. NICo
uses that pair to select the corresponding definition from the catalog. A
definition can contain several components, such as BMC, UEFI, CPLD, HGX BMC,
GPU, NIC, or CX7 firmware.

A definition contains model-level settings and one or more component entries:

| Setting | Scope | Meaning |
|---|---|---|
| `vendor` | Model | BMC vendor to which the definition applies. |
| `model` | Model | Host model to which the definition applies. Together, `vendor` and `model` identify the definition. |
| `components` | Model | Groups the component definitions, keyed by component type. |
| `ordering` | Model | Lists the components in the order NICo should update them. |
| `explicit_start_needed` | Model | When enabled, drift alone does not start the update. |
| `type` | Component | Identifies the component as BMC, UEFI, CEC, NIC, CPLD, HGX BMC, GPU, CX7, or a combined BMC and UEFI package. In legacy TOML, the type is the key below `components`. |
| `current_version_reported_as` | Component | Regular expression used to find the component in the firmware inventory reported by the BMC. The API derives this matcher from the vendor, model, and component type. |
| `preingest_upgrade_when_below` | Component | Sets the threshold for pre-ingestion firmware work. A lower reported version triggers an update when global automatic updates are enabled. |
| `known_firmware` | Component | Lists the versions NICo can install and the artifacts used to install them. |
| `version` | Firmware version | Version string NICo expects the firmware inventory to report after installation. |
| `default` | Firmware version | Marks the desired version used for drift detection after ingestion. |
| `preingestion_exclusive_config` | Firmware version | Marks a version for use during pre-ingestion without making it the steady-state default. |
| `files` | Firmware version | Structured list of firmware artifacts. Each entry provides a filename or URL and can include a SHA-256 digest. The API exposes this list as `artifacts`. |
| `install_only_specified` | Firmware version | Tells Redfish to install only the component identified by the firmware definition when the package contains multiple components. |
| `power_drains_needed` | Firmware version | Number of full power-drain cycles required to activate the firmware after installation. |
| `pre_update_resets` | Firmware version | Requests the platform-specific reset sequence before NICo starts the installation. |
| `script` | Firmware version | Legacy path that hands the update to a local script instead of using the normal Redfish upload path. |

`current_version_reported_as` is matched against firmware inventory entry
identifiers, not against version strings. For example,
`^Installed-.*__iDRAC.` selects the iDRAC entry from a Dell Redfish firmware
inventory. NICo then reads the installed version from that matching entry.

Legacy metadata also accepts the older artifact fields `filename`, `filenames`,
`url`, and `checksum`; new API entries use URL-based `artifacts`. The data model
still parses `mandatory_upgrade_from_priority` and `scout`, but the current
upgrade workflow does not use them as firmware-policy controls, so they are not
listed as active settings here.

### Minimum and default versions

The pre-ingestion minimum is an upgrade trigger, not the upgrade target. When a
host is below the minimum and global automatic updates are enabled, NICo
normally installs the version marked as the default. A firmware version can
instead set `preingestion_exclusive_config = true` when the platform needs a
different target during ingestion.

For example, assume the BMC minimum is `7.00.00.00` and the default is
`7.10.30.00`:

| Reported version | Result |
|---|---|
| `6.50.00.00` | Below the minimum. With automatic updates enabled, NICo updates the BMC during pre-ingestion, using `7.10.30.00` as the target. |
| `7.05.00.00` | Meets the minimum, so ingestion can continue. After ingestion it is still drifted from `7.10.30.00`. |
| `7.10.30.00` | Meets the minimum and matches the desired version. No firmware update is needed. |

Refer to [Pre-ingestion Firmware Updates](pre-ingestion.md) for minimum-version enforcement
and [Managed Host Firmware Updates](host-firmware.md) for post-ingestion drift handling.

### Configure host firmware through the API

The current way to configure host firmware is the
[Create or Update Host Firmware Config](api:PUT/v2/org/:org/nico/firmware-config/host)
operation:

```text
PUT /v2/org/:org/nico/firmware-config/host
```

The request is site-scoped and keyed by `(vendor, model)`. NICo stores the
configuration in `host_firmware_config` and uses it on subsequent discovery,
pre-ingestion, drift detection, and upgrade passes. This allows a site to
change its firmware catalog without changing the NICo deployment or placing
metadata files on the Core filesystem.

The endpoint requires Provider Admin authorization. The following request
defines one BMC version for a Dell PowerEdge R760:

```bash
curl -X PUT "https://<api-url>/v2/org/<org>/nico/firmware-config/host" \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{
    "siteId": "<site-id>",
    "vendor": "Dell",
    "model": "PowerEdge R760",
    "ordering": ["BMC"],
    "components": [
      {
        "type": "BMC",
        "preingestUpgradeWhenBelow": "7.00.00.00",
        "firmware": [
          {
            "version": "7.10.30.00",
            "default": true,
            "artifacts": [
              { "url": "https://<firmware-repository>/<artifact>" }
            ]
          }
        ]
      }
    ]
  }'
```

The request does not contain an inventory-matching regular expression. Core
derives that expression from its supported vendor, model, and component
mappings. The request is rejected when no mapping exists.

`PUT` is an upsert, not a complete replacement:

- The first request for a vendor and model must provide an ordering that
  contains every configured component.
- Each component must have exactly one default firmware version after the
  request is merged.
- Later requests can add components or firmware versions. Versions are merged
  by version string, and omitted components remain unchanged.
- Marking a new version as the default clears the previous default for that
  component.

The
[Delete Host Firmware Config](api:DELETE/v2/org/:org/nico/firmware-config/host)
operation removes the runtime entry. If the same vendor and model also exists
in static or legacy metadata, that lower-priority definition becomes effective
again.

### Legacy `metadata.toml` configuration

Before the Host Firmware Config API was introduced, host firmware definitions
were deployed as `metadata.toml` files. This is the legacy configuration path.
It remains supported for existing deployments, but new host firmware
definitions should use the API.

`firmware_global.firmware_directory` points to a directory containing one
subdirectory per metadata entry. NICo looks for a file named `metadata.toml` in
each of those subdirectories. For example:

```text
/opt/nico/firmware/
`-- dell-r760-bmc-7.10.30/
    `-- metadata.toml
```

Each file describes one vendor and model and can define one or more firmware
components. The equivalent legacy definition for the API example above is:

```toml
vendor = "Dell"
model = "PowerEdge R760"
ordering = ["bmc"]

[components.bmc]
current_version_reported_as = "^Installed-.*__iDRAC."
preingest_upgrade_when_below = "7.00.00.00"

[[components.bmc.known_firmware]]
version = "7.10.30.00"
url = "https://<firmware-repository>/<artifact>"
default = true
```

NICo also supports host definitions embedded in the static `host_models`
configuration. This is the base catalog supplied with the NICo deployment and
uses the same firmware data model as `metadata.toml`.

NICo builds the effective host firmware catalog in this order:

| Applied | Source |
|---|---|
| First | Static `host_models` configuration. |
| Next | Legacy `metadata.toml` files under `firmware_global.firmware_directory`. |
| Last (highest precedence) | Runtime `host_firmware_config` entries created through the API. |

Higher-priority sources overlay matching vendor, model, and component entries;
they do not discard unrelated lower-priority entries. For example, if a
`metadata.toml` file defines BMC `7.00.00.00` as the default and an API request
adds BMC `7.10.30.00` as the default, both remain known versions, but
`7.10.30.00` becomes the effective target.

Use the CLI to inspect the effective default versions after all three sources
have been merged:

```bash
nico-admin-cli -a <core-api-url> firmware show
```

### ConnectX-7 InfiniBand Firmware

NICo v2.0 and later use Scout to update CX7 firmware on hosts reported with
vendor `Nvidia`, model `DGXH100`, and firmware inventory entries named
`CX7_<number>`.

To prepare the update:

1. Publish the NVIDIA-signed CX7 firmware artifact at an HTTPS URL that Scout can
   access from the discovery environment.

   Use the
   [DGX H100/H200 firmware guide](https://docs.nvidia.com/dgx/dgxh100-fw-update-guide/network-card-fw-update.html)
   to select an artifact compatible with every CX7 adapter in the host. The
   artifact's SHA-256 digest is optional; when provided, it must be 64
   hexadecimal characters and NICo uses it to verify the download. NICo does
   not require a particular repository path or filename.

1. Call the
   [Host Firmware Config API](#configure-host-firmware-through-the-api) with the
   following:

   - `vendor: Nvidia`
   - `model: DGXH100`
   - Component `type: Cx7`
   - `version: 28.47.2682`
   - `default: true`
   - `powerDrainsNeeded: 1`
   - One artifact containing the HTTPS `url`
   - `sha256`: (optional) the artifact's 64-character SHA-256 digest to enable
     download verification

Use [Monitor and verify](host-firmware.md#monitor-and-verify). A host is updated
only when every `CX7_<number>` entry reports the target version, the host has
returned to `Ready` or `Assigned/Ready`, and its reprovisioning request is gone.

<Note>
**Legacy NICo installations:** If you are running a version of NICo prior to
2.0, the firmware container must also carry the CX7 upgrade script. NICo 2.0 and
later bundle the Scout script, so their firmware containers no longer need to
contain that script.
</Note>

Other host models require their own inventory mapping and Scout script. To add
support for another model:

- Follow the [contributing guide](../../../CONTRIBUTING.md).
- Add the catalog component regex for `FirmwareComponentType::Cx7` to the
  [host firmware inventory mappings](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-core/src/handlers/firmware.rs)
  for the model.
- Add the model's script and metadata under the
  [Scout firmware script assets](https://github.com/dsx-ai-factory/infra-controller/tree/main/pxe/scout-firmware-scripts).

The existing DGX H100 script might work on other servers, but it has not been
tested on them; validate it on the new platform before reusing it.

## DPU firmware

DPU firmware does not use the Host Firmware Config API. Its site configuration
has two related parts: an accepted-version list that triggers automatic work,
and a model catalog used to verify the result.

| Setting | Default | Meaning |
|---|---|---|
| `dpu_config.dpu_nic_firmware_initial_update_enabled` | `false` | Compatibility setting for initial DPU setup. The current state machine records it but does not use it to suppress BFB installation or verification. |
| `dpu_config.dpu_nic_firmware_reprovision_update_enabled` | `true` | Enables automatic DPU NIC drift selection in Machine Update Manager. Explicit DPU reprovisioning remains available when disabled. |
| `dpu_config.dpu_nic_firmware_update_versions` | Built-in BF2 and BF3 NIC versions | Accepted NIC versions used by the classic automatic drift detector. |
| `dpu_config.dpu_models` | Built-in BF2 and BF3 definitions | BMC, ERoT/CEC, NIC, and UEFI baselines associated with each DPU model. |
| `dpu_config.dpu_enable_secure_boot` | `false` | Allows classic reprovisioning to install the BFB through Redfish and enable secure boot when every attached DPU supports that path. Otherwise NICo uses UEFI HTTP network installation. |

### Automatic DPU NIC trigger

NICo checks the reported DPU NIC version against
`dpu_config.dpu_nic_firmware_update_versions`:

```toml
[dpu_config]
dpu_nic_firmware_update_versions = ["32.43.1014", "32.44.1030"]
```

This is an accepted-version list rather than a single default. A DPU reporting
either version in the example is considered compliant. A version outside the
list is drifted and, when DPU NIC updates are enabled, makes the DPU eligible
for reprovisioning. Allowing more than one version is useful during a staged
rollout where both the old and new versions must temporarily remain valid.

`dpu_nic_firmware_reprovision_update_enabled` enables this automatic drift
module. It does not prevent an operator from requesting DPU reprovisioning.

### Post-reprovision baseline

The `dpu_models` catalog describes the component versions expected after a
classic DPU reprovision. Entries are keyed by NICo's normalized DPU model,
currently `bluefield2` or `bluefield3`, and use the same firmware structures as
the static host catalog:

```toml
[dpu_config.dpu_models.bluefield3]
vendor = "Nvidia"
model = "Bluefield 3 SmartNIC Main Card"
ordering = ["bmc", "cec"]

[dpu_config.dpu_models.bluefield3.components.nic]
current_version_reported_as = "DPU_NIC"

[[dpu_config.dpu_models.bluefield3.components.nic.known_firmware]]
version = "32.44.1030"
default = true
```

The built-in catalog defines BMC, CEC/ERoT, NIC, and UEFI entries for each
model. The current reprovisioning verifier compares BMC, CEC/ERoT, and NIC. It
selects the last non-pre-ingestion-only known version for each component; in
normal configurations, keep one steady-state entry for each component.

NICo applies these versions by installing or booting the deployed BlueField
Bundle, not by uploading the `known_firmware` entries as independent packages.
The accepted NIC list, DPU model catalog, and firmware carried by `forge.bfb`
must therefore be updated together. For example, if the BFB installs NIC
version `32.44.1030`, that version must be accepted by
`dpu_nic_firmware_update_versions` and should be the NIC baseline in the
matching `dpu_models` entry.

Refer to [DPU firmware updates](dpu-firmware.md) for how NICo acts on this
configuration.

## Operational controls

The settings in this section are fields in NICo Core's site configuration. They
control the automatic paths; they do not define component target versions.

### Managed-machine scheduling

Machine Update Manager uses one capacity budget for managed-host and DPU
firmware work:

| Setting | Default | Meaning |
|---|---|---|
| `machine_update_run_interval` | `300` | Seconds between Machine Update Manager passes. |
| `machine_updater.max_concurrent_machine_updates_absolute` | unset | Maximum number of distinct hosts that may be in machine-update workflows. Set this to a positive value to allow automatic managed-host or DPU work. |
| `machine_updater.max_concurrent_machine_updates_percent` | unset | Intended percentage-based capacity. The current implementation calculates this incorrectly; leave it unset and use the absolute limit. |
| `machine_updater.instance_autoreboot_period.start` | unset | Start of a one-time UTC window in which an assigned host may consume an existing update request without tenant approval. |
| `machine_updater.instance_autoreboot_period.end` | unset | End of that one-time UTC window. |

For example, this allows at most five managed hosts to be in update workflows
and gives assigned hosts a four-hour automatic reboot window:

```toml
machine_update_run_interval = 300

[machine_updater]
max_concurrent_machine_updates_absolute = 5
instance_autoreboot_period.start = "2026-08-15T01:00:00Z"
instance_autoreboot_period.end = "2026-08-15T05:00:00Z"
```

The autoreboot period is not the per-host firmware window used by models with
`explicit_start_needed`. Operators open that window through the managed-host
API. Refer to [Scheduling gates](host-firmware.md#scheduling-gates).

### Firmware execution

`firmware_global` controls host firmware execution and the separate
pre-ingestion manager:

| Setting | Default | Meaning |
|---|---|---|
| `firmware_global.autoupdate` | `false` | Enables pre-ingestion upgrades and supplies the default automatic-update policy for managed hosts. A per-machine policy can override the managed-host default. |
| `firmware_global.run_interval` | `30s` | Interval between pre-ingestion manager passes. |
| `firmware_global.max_uploads` | `4` | Shared limit for concurrent host firmware uploads. |
| `firmware_global.concurrency_limit` | `16` | Maximum number of pre-ingestion endpoints processed concurrently. This is separate from Machine Update Manager capacity. |
| `firmware_global.firmware_directory` | `/opt/nico/firmware`, otherwise `/opt/carbide/firmware` | Directory containing legacy metadata and local firmware artifacts. |
| `firmware_global.firmware_download_cache_directory` | `/mnt/persistence/fw/download-cache` | Writable cache for downloaded firmware artifacts. |
| `firmware_global.host_firmware_upgrade_retry_interval` | `60m` | Delay before the managed-host state machine retries a failed host firmware upgrade. |
| `firmware_global.instance_updates_manual_tagging` | See below | When `true`, automatic host selection is limited to unassigned hosts in top-level `Ready`. When `false`, assigned hosts can also receive a pending request. |
| `firmware_global.no_reset_retries` | `false` | Disables the normal retry handling for BMC reset failures during pre-ingestion and managed-host firmware work. |
| `firmware_global.hgx_bmc_gpu_reboot_delay` | `30s` | Delay after a GPU reboot before NICo accesses the HGX BMC again. |
| `firmware_global.requires_manual_upgrade` | `false` | Adds a manually confirmed upgrade gate for MNNVL-capable hosts. |
| `firmware_global.max_concurrent_bfb_copies` | `10` | Maximum concurrent BFB copies performed by the pre-ingestion manager. |

Set `instance_updates_manual_tagging` explicitly. When a present
`firmware_global` table omits the field, deserialization uses `true`; when the
entire table is omitted, the current whole-structure default uses `false`.

A staging configuration can make the policy explicit while leaving site-wide
automatic selection disabled:

```toml
[firmware_global]
autoupdate = false
run_interval = "30s"
max_uploads = 4
concurrency_limit = 16
host_firmware_upgrade_retry_interval = "60m"
instance_updates_manual_tagging = true
requires_manual_upgrade = false
max_concurrent_bfb_copies = 10
```

The legacy `host_enable_autoupdate` and `host_disable_autoupdate` fields do not
provide a reliable model allowlist. Use the
[per-machine automatic-update policy](host-firmware.md#enable-or-disable-one-host)
instead.

## Roll out a new baseline

Treat a baseline change as a site-wide rollout, even when the first update is a
canary:

1. Update the canonical configuration source. Do not maintain a second live
   version table in this guide or a site runbook.
1. Make every referenced host artifact available. For DPU firmware, update the
   BFB, accepted NIC versions, and `dpu_models` together.
1. Check host component ordering, defaults, pre-ingestion thresholds, and
   inventory matchers. Then inspect the merged host catalog with `firmware
   show`.
1. Configure a nonzero shared capacity, explicit-start windows, and assigned-host
   approval policy. Keep site-wide automatic selection disabled while validating
   one host through a per-machine host policy or an explicit DPU reprovisioning
   request.
1. After verification, enable the intended site-wide automatic policy.
1. Monitor the applicable workflow until inventory reports the configured
   versions and the host has returned to service.

Changing a pre-ingestion threshold while `firmware_global.autoupdate` is enabled
can affect every matching endpoint on the next pre-ingestion pass. Validate the
catalog and artifact availability before applying that change.

## Rack and component firmware

Rack and component firmware do not use either of the configuration models
above. Their target version comes from the update request or from the default
selected by the rack or component backend. Refer to
[Rack and Tray Firmware Updates](rack-component-firmware.md).

### Rack profile firmware object

A rack profile can specify one firmware-object JSON document to use as the
default input for automatic rack firmware and switch NVOS updates:

```toml
[rack_profiles.NVL72]
product_family = "gb200"

[rack_profiles.NVL72.firmware_object]
url = "https://firmware.example.com/objects/nvl72.json"
fetch_timeout = "30s"

[rack_profiles.NVL72.rack_capabilities.compute]
vendor = "NVIDIA"
count = 18

[rack_profiles.NVL72.rack_capabilities.switch]
vendor = "NVIDIA"
count = 9

[rack_profiles.NVL72.rack_capabilities.power_shelf]
vendor = "LiteOn"
count = 8
```

When `firmware_object` is configured for a profile with switches, the SOT JSON
must contain an NVOS image whose firmware type matches `rack_hardware_class`.
NICo requests `prod` when `rack_hardware_class` is omitted. RMS records an
asynchronous update failure when the document does not contain the required
image. After all NVOS image jobs complete or fail, NICo uses RMS to restore the
desired NVOS admin password on every selected switch before the rack leaves the
NVOS update phase. Without `firmware_object`, NICo skips both automatic update
phases. An explicit maintenance request can supply a firmware object instead.
If no firmware object is available while a switch in the maintenance scope is
already waiting for an NVOS update, the rack transitions to `Error` instead of
skipping the NVOS phase.

The `url` field identifies the document location. The optional `fetch_timeout`
field accepts duration strings such as `30s` and `60s` and defaults to `30s`.
Use seconds for this request timeout, although the parser accepts other
duration units such as milliseconds (`ms`), minutes (`m`), and hours (`h`).
