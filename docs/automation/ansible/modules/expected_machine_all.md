# nvidia.infra_controller.expected_machine_all – manage Expected Machine

Replace all Expected Machines

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `expected_machines` | `list` | No | Full desired Expected Machine set; BMC MAC addresses and chassis serial numbers must be unique. Required when creating a new resource. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `site_id` | `str` | No | ID of the Site whose Expected Machines should be replaced. Required when creating a new resource. Required when deleting the resource. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |

## Examples

```yaml
- name: Create a Expected Machine
  nvidia.infra_controller.expected_machine_all:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    expected_machines: "{{ expected_machines }}"
    site_id: "{{ site_id }}"

- name: Delete a Expected Machine
  nvidia.infra_controller.expected_machine_all:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    site_id: "{{ site_id }}"
```
