# nvidia.infra_controller.expected_rack_all – manage Expected Rack

Replace all Expected Racks

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `expected_racks` | `list` | No | The full desired set of Expected Racks for the Site. Every entry must reference the same `siteId` as the top-level field, and `rackId` values must be unique within the request. May be empty to clear all Expected Racks for the Site. Required when creating a new resource. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `site_id` | `str` | No | ID of the Site whose Expected Racks should be replaced. Required when creating a new resource. Required when deleting the resource. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |

## Examples

```yaml
- name: Create a Expected Rack
  nvidia.infra_controller.expected_rack_all:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    expected_racks: "{{ expected_racks }}"
    site_id: "{{ site_id }}"

- name: Delete a Expected Rack
  nvidia.infra_controller.expected_rack_all:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    site_id: "{{ site_id }}"
```
