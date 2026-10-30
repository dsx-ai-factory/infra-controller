# nvidia.infra_controller.expected_rack_group_all – manage Expected Rack Group

Replace all Expected Rack Groups

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `expected_rack_groups` | `list` | No | The full desired set of Expected Rack Groups for the Site. Every entry must reference the same `siteId` as the top-level field, and `rackGroupId` values must be unique within the request. Required and non-null. Use an explicit empty array to clear all Expected Rack Groups for the Site. Required when creating a new resource. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `site_id` | `str` | No | ID of the Site whose Expected Rack Groups should be replaced. Required when creating a new resource. Required when deleting the resource. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |

## Examples

```yaml
- name: Create a Expected Rack Group
  nvidia.infra_controller.expected_rack_group_all:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    expected_rack_groups: "{{ expected_rack_groups }}"
    site_id: "{{ site_id }}"

- name: Delete a Expected Rack Group
  nvidia.infra_controller.expected_rack_group_all:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    site_id: "{{ site_id }}"
```
