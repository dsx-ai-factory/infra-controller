# nvidia.infra_controller.machine_health_report – manage Machine

Retrieve all Machine health reports

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `alerts` | `list` | No | Results from failed health probes for the Machine. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `machine_id` | `str` | Yes | ID path parameter: machine_id. |
| `mode` | `str` | No | How updates to this health report should be handled Choices: `Merge`, `Replace`. |
| `source` | `str` | No | Health report source. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `successes` | `list` | No | Results from successful health probes for the Machine. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Machine
  nvidia.infra_controller.machine_health_report:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    machine_id: "{{ machine_id }}"

- name: Delete a Machine
  nvidia.infra_controller.machine_health_report:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    id: "{{ resource_id }}"
    machine_id: "{{ machine_id }}"
```
