# nvidia.infra_controller.expected_switch_all – manage Expected Switch

Replace all Expected Switches

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `expected_switches` | `list` | No | Full desired Expected Switch set; BMC MAC addresses, serial numbers, and NVOS MAC addresses must be unique. Required when creating a new resource. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `site_id` | `str` | No | ID of the Site whose Expected Switches should be replaced. Required when creating a new resource. Required when deleting the resource. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |

## Examples

```yaml
- name: Create a Expected Switch
  nvidia.infra_controller.expected_switch_all:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    expected_switches: "{{ expected_switches }}"
    site_id: "{{ site_id }}"

- name: Delete a Expected Switch
  nvidia.infra_controller.expected_switch_all:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    site_id: "{{ site_id }}"
```
