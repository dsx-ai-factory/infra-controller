# nvidia.infra_controller.task_run – manage Task Run

Create Task Runs.

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `description` | `str` | No | Optional free-form description. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `name` | `str` | Yes | Human-readable name of the Task Run. |
| `operation` | `dict` | Yes | The operation the Task Run executes. Firmware is the only supported operation. |
| `options` | `dict` | Yes | Execution policy for the Task Run. |
| `selector` | `dict` | No | Narrows the candidate Racks. Omit to target the full candidate scope (100%). |
| `site_id` | `str` | Yes | ID of the Site to create the Task Run on. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Task Run
  nvidia.infra_controller.task_run:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-task-run"
    operation: "{{ operation }}"
    options: "{{ options }}"
    site_id: "{{ site_id }}"

```
