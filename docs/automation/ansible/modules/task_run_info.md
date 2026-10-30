# nvidia.infra_controller.task_run_info – query Task Run

Retrieve all Task Runs

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `operation_type` | `str` | No | Filter by operation type. Choices: `PowerControl`, `FirmwareControl`. |
| `site_id` | `str` | Yes | ID of the Site that owns the Task Runs (Task Runs are site-scoped). |
| `status` | `str` | No | Filter by Task Run status. Choices: `Pending`, `Running`, `Paused`, `Completed`, `Cancelled`, `Failed`, `CompletedWithFailures`. |

## Examples

```yaml
- name: List all Task Run resources
  nvidia.infra_controller.task_run_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"

- name: Get a specific Task Run by ID
  nvidia.infra_controller.task_run_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"
    id: "{{ resource_id }}"
```
