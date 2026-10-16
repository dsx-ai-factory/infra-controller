# nvidia.infra_controller.expected_rack_group_all – manage Expected Rack Group

Replace all Expected Rack Groups

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `expected_rack_groups` | `list` | No | The full desired set of Expected Rack Groups for the Site. Every entry must reference the same `siteId` as the top-level field, and `rackGroupId` values must be unique within the request. Required and non-null. Use an explicit empty array to clear all Expected Rack Groups for the Site. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `site_id` | `str` | No | ID of the Site whose Expected Rack Groups should be replaced |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Expected Rack Group
  nvidia.infra_controller.expected_rack_group_all:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present

- name: Delete a Expected Rack Group
  nvidia.infra_controller.expected_rack_group_all:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    id: "{{ resource_id }}"
```
