# nvidia.infra_controller.expected_switch_all – manage Expected Switch

Replace all Expected Switches

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `expected_switches` | `list` | No | Full desired Expected Switch set; BMC MAC addresses, serial numbers, and NVOS MAC addresses must be unique |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `site_id` | `str` | No | ID of the Site whose Expected Switches should be replaced |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Expected Switch
  nvidia.infra_controller.expected_switch_all:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present

- name: Delete a Expected Switch
  nvidia.infra_controller.expected_switch_all:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    id: "{{ resource_id }}"
```
