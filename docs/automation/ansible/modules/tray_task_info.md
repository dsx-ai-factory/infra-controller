# nvidia.infra_controller.tray_task_info – query Tray

Retrieve all Tasks for a Tray

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `active_only` | `bool` | No | Restrict results to non-terminal Tasks. |
| `id` | `str` | Yes | ID path parameter: id. |
| `include_report` | `bool` | No | Include the per-task execution report on each returned task. |
| `site_id` | `str` | Yes | ID of the Site that owns the Tray. |

## Examples

```yaml
- name: List all Tray resources
  nvidia.infra_controller.tray_task_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"

- name: Get a specific Tray by ID
  nvidia.infra_controller.tray_task_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"
    id: "{{ resource_id }}"
```
