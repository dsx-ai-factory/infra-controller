# nvidia.infra_controller.task_rule_info – query Rule

List Operation Rules

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `operation_type` | `str` | No | Filter by operation type. Choices: `PowerControl`, `FirmwareControl`. |
| `site_id` | `str` | Yes | ID of the Site that owns the rules (rules are site-scoped). |

## Examples

```yaml
- name: List all Rule resources
  nvidia.infra_controller.task_rule_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"

- name: Get a specific Rule by ID
  nvidia.infra_controller.task_rule_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"
    id: "{{ resource_id }}"
```
