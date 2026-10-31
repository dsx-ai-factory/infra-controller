# nvidia.infra_controller.domain_nvlink_power – manage Domain

Power control NVLink Domains

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | Yes | ID of the resource. Used for lookup. |
| `override_readiness_check` | `bool` | No | When true, proceed even if one or more target components (or hosts on the owning rack for rack-scoped components) are reported as not ready by their persisted status. Intended for operator-supervised maintenance. |
| `rule_id` | `str` | No | Optional Operation Rule UUID. When set, pins this operation to the named rule and overrides Flow's default rule resolution. |
| `site_id` | `str` | Yes | ID of the Site |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |

## Examples

```yaml
- name: Power control NVLink Domains
  nvidia.infra_controller.domain_nvlink_power:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    id: "{{ resource_id }}"
    site_id: "{{ site_id }}"
```
