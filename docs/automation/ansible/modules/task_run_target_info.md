# nvidia.infra_controller.task_run_target_info – query Task Run

Retrieve all Task Run Targets

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | Yes | ID path parameter: id. |
| `phase_scope` | `str` | No | Restrict targets to the current phase, completed phases, or both. Choices: `currentPhase`, `completedPhases`, `currentAndCompletedPhases`. |
| `site_id` | `str` | Yes | ID of the Site that owns the Task Run. |
| `status` | `str` | No | Filter by target status. Choices: `Pending`, `Blocked`, `Submitted`, `Completed`, `Failed`, `Terminated`, `Skipped`, `Claimed`. |

## Examples

```yaml
- name: List all Task Run resources
  nvidia.infra_controller.task_run_target_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"

- name: Get a specific Task Run by ID
  nvidia.infra_controller.task_run_target_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"
    id: "{{ resource_id }}"
```
