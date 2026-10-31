# nvidia.infra_controller.domain_nvlink_firmware – manage Domain

Firmware update NVLink Domains

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | Yes | ID of the resource. Used for lookup. |
| `override_readiness_check` | `bool` | No | When true, proceed even if one or more target components (or hosts on the owning rack for rack-scoped components) are reported as not ready by their persisted status. Intended for operator-supervised maintenance. |
| `override_version_check` | `bool` | No | When true, request that the selected component backend override firmware version-based checks when deciding whether to apply the update. This permits same-version reapplication and downgrade when supported. It does not bypass readiness checks or state-controller routing. |
| `rule_id` | `str` | No | Optional Operation Rule UUID. When set, pins every task spawned by this operation to the named rule and overrides Flow's default rule resolution. |
| `site_id` | `str` | Yes | ID of the Site |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |
| `version` | `str` | No | Target firmware input. NVLink Domain targets resolve to their member racks. For rack-scale RMS components, an omitted, null, empty, or whitespace-only value uses the owning rack profile's configured firmware object. Resolution or fetch failures are reported by the asynchronous task. |

## Examples

```yaml
- name: Firmware update NVLink Domains
  nvidia.infra_controller.domain_nvlink_firmware:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    id: "{{ resource_id }}"
    site_id: "{{ site_id }}"
```
