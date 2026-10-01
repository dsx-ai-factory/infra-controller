# nvidia.infra_controller.expected_power_shelf_all – manage Expected Power Shelf

Replace all Expected Power Shelves

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `expected_power_shelves` | `list` | No | Full desired Expected Power Shelf set; BMC MAC addresses and shelf serial numbers must be unique |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `site_id` | `str` | No | ID of the Site whose Expected Power Shelves should be replaced |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Expected Power Shelf
  nvidia.infra_controller.expected_power_shelf_all:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present

- name: Delete a Expected Power Shelf
  nvidia.infra_controller.expected_power_shelf_all:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    id: "{{ resource_id }}"
```
