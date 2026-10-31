# nvidia.infra_controller.task_cancel – manage Task

Cancel a Task

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | Yes | ID of the resource. Used for lookup. |
| `site_id` | `str` | Yes | ID of the Site that owns the task (tasks are site-scoped). |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |

## Examples

```yaml
- name: Cancel a Task
  nvidia.infra_controller.task_cancel:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    id: "{{ resource_id }}"
    site_id: "{{ site_id }}"

```
