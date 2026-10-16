# nvidia.infra_controller.machine_health_report_info – query Machine

Retrieve all Machine health reports

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `machine_id` | `str` | Yes | ID path parameter: machine_id. |

## Examples

```yaml
- name: List all Machine resources
  nvidia.infra_controller.machine_health_report_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    machine_id: "{{ machine_id }}"

- name: Get a specific Machine by ID
  nvidia.infra_controller.machine_health_report_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    machine_id: "{{ machine_id }}"
    id: "{{ resource_id }}"
```
