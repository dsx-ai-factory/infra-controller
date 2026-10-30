# nvidia.infra_controller.tray_firmware – manage Tray

Manage Tray resources.

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `authentication_data` | `dict` | No | Optional, write-only authentication data for firmware downloads. Not supported for DPU-only updates or by the legacy NICo compute firmware controller. |
| `filter` | `dict` | No | Filter that selects Trays targeted for firmware update |
| `id` | `str` | No | ID of the resource. When provided, targets a single resource. |
| `override_readiness_check` | `bool` | No | When true, proceed even if one or more target components (or hosts on the owning rack for rack-scoped components) are reported as not ready by their persisted status. Intended for operator-supervised maintenance. |
| `override_version_check` | `bool` | No | When true, request that the selected component backend override firmware version-based checks when deciding whether to apply the update. This permits same-version reapplication and downgrade when supported. It does not bypass readiness checks or state-controller routing. |
| `rule_id` | `str` | No | Optional Operation Rule UUID. When set, pins this firmware update to the named rule and overrides Flow's default rule resolution. |
| `site_id` | `str` | No | ID of the Site |
| `targets` | `list` | No | Optional subset of firmware targets to update within the targeted tray. Names are lowercase and select sub-parts of the tray, such as BMC and BIOS. The accepted set per tray type comes from the Flow service's NICo proto bindings (which mirror Core's per-tray-type enums in `crates/rpc/proto/forge.proto`), so the supported values track Core as new sub-parts are added: - switch trays (NvSwitchComponent): bmc, cpld, bios, nvos - powershelf trays (PowerShelfComponent): pmc, psu - compute trays (ComputeTrayComponent): bmc, bios The default `nico` compute component manager forwards BMC and BIOS selections through `UpdateComponentFirmware`. The opt-in `nicolegacy` manager schedules the whole configured host firmware bundle; it does not honor BMC or BIOS sub-target selection. Omitted or empty means "update everything in the bundle" (the historical default) for compute-tray-internal targets. Unknown names are rejected. The special target `dpu`, valid only on compute trays, requests DPU reprovisioning on the matched host. Unlike the other targets, `dpu` is NOT covered by the "omitted/empty means everything" default — it must be listed explicitly. `version` is ignored on the `dpu` branch; the target firmware version comes from site configuration. |
| `version` | `str` | No | Target firmware input serialized as a string. Its format is a contract between the caller and the component backend selected for the target. The REST API and Flow orchestration otherwise preserve this value. Flow only unwraps the optional per-tray mapping described below before passing the value to the selected component manager. For compatibility, when Flow's compute component manager is explicitly configured as `nicolegacy` (the default is `nico`), use a compute tray endpoint without `version` or `targets` for on-demand host updates. Core selects the configured host firmware bundle. For RMS-backed rack-scale firmware updates, use the complete SOT firmware-object JSON document, not a release version such as `24.11.0`. A SOT document is an object with `ProductName` and `Milestones` at the top level. Each milestone contains `Name`, `State`, and `BoardSKUs`; each board SKU contains `Name`, `Type`, and `Components`. Component groups contain `Software` and `Firmware` entries describing `Component`, `Version`, `Type`, `FileNames`, artifact `Locations`, and optional `SubComponents`. Obtain the complete document from the SOT release process; the abbreviated examples are not executable input. Supply a complete document covering all component types selected by the operation rule. The string can contain either one shared firmware input for all selected trays or a JSON mapping from tray type (`compute`, `nvswitch`, `powershelf`) to firmware input. Each mapping value must satisfy its selected backend's contract. Flow forwards object values as raw JSON and unquotes string values. These exact lowercase top-level keys are reserved for per-tray mappings; a shared JSON object must not contain any of them. A missing tray type in the mapping skips that component type. A non-empty, non-layered value is forwarded unchanged. For rack-scale RMS components, an omitted, null, empty, or whitespace-only value uses the owning rack profile's configured firmware object. Resolution or fetch failures are reported by the asynchronous task. |

## Examples

```yaml
- name: Run firmware on all tray resources
  nvidia.infra_controller.tray_firmware:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"

- name: Run firmware on a specific tray
  nvidia.infra_controller.tray_firmware:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
    site_id: "{{ site_id }}"
```
