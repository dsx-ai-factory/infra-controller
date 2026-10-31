# nvidia.infra_controller.task_run_advance – manage Task Run

Advance a Task Run to its next phase

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `expected_phase_index` | `int` | No | Optional guard: when set, the phase that would be opened must match this zero-based index, otherwise the advance is rejected. |
| `id` | `str` | Yes | ID of the resource. Used for lookup. |
| `site_id` | `str` | Yes | ID of the Site that owns the Task Run. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |

## Examples

```yaml
- name: Advance a Task Run to its next phase
  nvidia.infra_controller.task_run_advance:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    id: "{{ resource_id }}"
    site_id: "{{ site_id }}"

```
