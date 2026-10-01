# nvidia.infra_controller.tray_power – manage Tray

Manage Tray resources.

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `filter` | `dict` | No | Filter that selects Trays whose power state should be updated |
| `id` | `str` | No | ID of the resource. When provided, targets a single resource. |
| `override_readiness_check` | `bool` | No | When true, proceed even if one or more target components (or hosts on the owning rack for rack-scoped components) are reported as not ready by their persisted status. Intended for operator-supervised maintenance. |
| `rule_id` | `str` | No | Optional Operation Rule UUID. When set, pins this operation to the named rule and overrides Flow's default rule resolution. |
| `site_id` | `str` | No | ID of the Site |
| `state` | `str` | No | Power control state to apply: - `On`: Power on the target(s) - `Off`: Graceful power off - `Cycle`: Graceful power cycle (restart) - `ForceOff`: Forced power off (immediate) - `ForceCycle`: Forced power cycle (immediate restart) - `ACPowerCycle`: Remove and restore AC power (unsupported on Viking systems) Exact lowercase forms are also accepted for compatibility. Choices: `On`, `Off`, `Cycle`, `ForceOff`, `ForceCycle`, `ACPowerCycle`, `on`, `off`, `cycle`, `forceoff`, `forcecycle`, `acpowercycle`. |

## Examples

```yaml
- name: Run power on all tray resources
  nvidia.infra_controller.tray_power:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"
    state: "{{ state }}"

- name: Run power on a specific tray
  nvidia.infra_controller.tray_power:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
    site_id: "{{ site_id }}"
    state: "{{ state }}"
```
