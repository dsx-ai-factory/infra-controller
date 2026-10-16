# nvidia.infra_controller.rack_task_info – query Rack

Retrieve all Tasks for a Rack

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `active_only` | `bool` | No | Restrict results to non-terminal Tasks. |
| `id` | `str` | Yes | ID path parameter: id. |
| `include_report` | `bool` | No | Include the per-task execution report on each returned task. |
| `site_id` | `str` | Yes | ID of the Site that owns the Rack. |

## Examples

```yaml
- name: List all Rack resources
  nvidia.infra_controller.rack_task_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"

- name: Get a specific Rack by ID
  nvidia.infra_controller.rack_task_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"
    id: "{{ resource_id }}"
```
