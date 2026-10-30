# nvidia.infra_controller.task_info – query Task

Retrieve all Tasks

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `active_only` | `bool` | No | Restrict results to non-terminal Tasks. |
| `id` | `str` | No | ID of the resource to retrieve. |
| `include_report` | `bool` | No | Include the per-task execution report on each returned Task. |
| `site_id` | `str` | Yes | ID of the Site whose Tasks are returned. |

## Examples

```yaml
- name: List all Task resources
  nvidia.infra_controller.task_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"

- name: Get a specific Task by ID
  nvidia.infra_controller.task_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"
    id: "{{ resource_id }}"
```
