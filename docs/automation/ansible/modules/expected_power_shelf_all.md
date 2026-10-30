# nvidia.infra_controller.expected_power_shelf_all – manage Expected Power Shelf

Replace all Expected Power Shelves

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `expected_power_shelves` | `list` | No | Full desired Expected Power Shelf set; BMC MAC addresses and shelf serial numbers must be unique. Required when creating a new resource. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `site_id` | `str` | No | ID of the Site whose Expected Power Shelves should be replaced. Required when creating a new resource. Required when deleting the resource. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |

## Examples

```yaml
- name: Create a Expected Power Shelf
  nvidia.infra_controller.expected_power_shelf_all:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    expected_power_shelves: "{{ expected_power_shelves }}"
    site_id: "{{ site_id }}"

- name: Delete a Expected Power Shelf
  nvidia.infra_controller.expected_power_shelf_all:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    site_id: "{{ site_id }}"
```
