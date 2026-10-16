# nvidia.infra_controller.task_run_cancel – manage Task Run

Cancel a Task Run

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | Yes | ID of the resource. Used for lookup. |
| `reason` | `str` | No | Optional free-form reason recorded with the cancellation. |
| `site_id` | `str` | No | ID of the Site that owns the Task Run. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Task Run
  nvidia.infra_controller.task_run_cancel:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present

```
