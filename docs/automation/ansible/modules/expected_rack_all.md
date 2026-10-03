# nvidia.infra_controller.expected_rack_all – manage Expected Rack

Replace all Expected Racks

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `expected_racks` | `list` | No | The full desired set of Expected Racks for the Site. Every entry must reference the same `siteId` as the top-level field, and `rackId` values must be unique within the request. May be empty to clear all Expected Racks for the Site. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `site_id` | `str` | No | ID of the Site whose Expected Racks should be replaced |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Expected Rack
  nvidia.infra_controller.expected_rack_all:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present

- name: Delete a Expected Rack
  nvidia.infra_controller.expected_rack_all:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    id: "{{ resource_id }}"
```
