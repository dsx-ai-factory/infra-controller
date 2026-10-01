# nvidia.infra_controller.task_run_advance – manage Task Run

Advance a Task Run to its next phase

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `expected_phase_index` | `int` | No | Optional guard: when set, the phase that would be opened must match this zero-based index, otherwise the advance is rejected. |
| `id` | `str` | Yes | ID of the resource. Used for lookup. |
| `site_id` | `str` | No | ID of the Site that owns the Task Run. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Task Run
  nvidia.infra_controller.task_run_advance:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present

```
