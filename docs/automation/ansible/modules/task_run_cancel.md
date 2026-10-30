# nvidia.infra_controller.task_run_cancel – manage Task Run

Cancel a Task Run

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | Yes | ID of the resource. Used for lookup. |
| `reason` | `str` | No | Optional free-form reason recorded with the cancellation. |
| `site_id` | `str` | Yes | ID of the Site that owns the Task Run. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |

## Examples

```yaml
- name: Cancel a Task Run
  nvidia.infra_controller.task_run_cancel:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    id: "{{ resource_id }}"
    site_id: "{{ site_id }}"

```
