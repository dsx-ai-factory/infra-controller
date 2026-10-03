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
| `targets` | `list` | No | Optional subset of firmware targets to update within the targeted tray. Names are lowercase and select sub-parts of the tray (BMC, BIOS, etc.). The accepted set per tray type comes from the Flow service's NICo proto bindings (which mirror Core's per-tray-type enums in `NICo-core/crates/rpc/proto/forge.proto`), so the supported values track Core as new sub-parts are added: - switch trays (NvSwitchComponent): currently bmc, cpld, bios, nvos - powershelf trays (PowerShelfComponent): currently pmc, psu - compute trays (ComputeTrayComponent): currently bmc, bios (currently NOT honored end-to-end: the NICo compute-firmware path goes through SetFirmwareUpdateTimeWindow + auto-update, which has no per-target selection; the request is logged and the whole bundle is applied. Will be honored once compute moves to UpdateComponentFirmware.) Omitted or empty means "update everything in the bundle" (the historical default) for compute-tray-internal targets. Unknown names are rejected. Requires `version` to be set. The special target `dpu`, valid only on compute trays, requests DPU reprovisioning on the matched host. Unlike the other targets, `dpu` is NOT covered by the "omitted/empty means everything" default — it must be listed explicitly. `version` is ignored on the `dpu` branch; the target firmware version comes from site configuration. |
| `version` | `str` | No | Firmware input serialized as a string: either one shared value for all selected trays or a JSON mapping from tray type (`compute`, `nvswitch`, `powershelf`) to firmware input. These exact lowercase top-level keys are reserved for per-tray mappings; a shared JSON object must not contain any of them. A missing tray type in the mapping receives an empty input, which does not guarantee a skipped update. Empty, null, or omitted input is handled by the selected backend and operation rule. |

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
