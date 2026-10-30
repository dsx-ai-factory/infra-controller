# nvidia.infra_controller.task_run_pause – manage Task Run

Pause a Task Run

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | Yes | ID of the resource. Used for lookup. |
| `site_id` | `str` | Yes | ID of the Site that owns the Task Run (Task Runs are site-scoped). |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |

## Examples

```yaml
- name: Pause a Task Run
  nvidia.infra_controller.task_run_pause:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    id: "{{ resource_id }}"
    site_id: "{{ site_id }}"

```
