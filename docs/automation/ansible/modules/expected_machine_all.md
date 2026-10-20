# nvidia.infra_controller.expected_machine_all – manage Expected Machine

Replace all Expected Machines

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `expected_machines` | `list` | No | Full desired Expected Machine set; BMC MAC addresses and chassis serial numbers must be unique |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `site_id` | `str` | No | ID of the Site whose Expected Machines should be replaced |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Expected Machine
  nvidia.infra_controller.expected_machine_all:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present

- name: Delete a Expected Machine
  nvidia.infra_controller.expected_machine_all:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    id: "{{ resource_id }}"
```
