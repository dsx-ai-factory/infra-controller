# nvidia.infra_controller.machine_validation_result_info – query Machine

Retrieve Machine validation results

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `machine_id` | `str` | Yes | ID path parameter: machine_id. |

## Examples

```yaml
- name: List all Machine resources
  nvidia.infra_controller.machine_validation_result_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    machine_id: "{{ machine_id }}"
```
