# nvidia.infra_controller.expected_rack_group – manage Expected Rack Group

Retrieve all Expected Rack Groups

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `description` | `str` | No | Human-readable description, at most 1024 UTF-8 bytes. An empty string is allowed. |
| `expected_rack_groups` | `list` | No | The full desired set of Expected Rack Groups for the Site. Every entry must reference the same `siteId` as the top-level field, and `rackGroupId` values must be unique within the request. Required and non-null. Use an explicit empty array to clear all Expected Rack Groups for the Site. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `labels` | `str` | No | User-defined key-value pairs with ASCII keys and Unicode values, each at most 255 UTF-8 bytes. Well-known keys (`chassis.*`, `location.*`) are used to convey chassis identity and physical location. Omission or null preserves existing labels; an empty object clears them. When labels is null, provide a non-null value for at least one of `rackGroupId`, `topology`, `racks`, `name`, or `description`. |
| `name` | `str` | No | ASCII human-readable name, at most 256 characters. An empty string is allowed. |
| `rack_group_id` | `str` | No | Operator-supplied rack group identifier. Immutable on update: omit this field, send `null`, or provide the existing value without changing the identity. A changed value is rejected because Core uses rackGroupId as the identity key. |
| `racks` | `list` | No | Ordered racks and their devices. Rack IDs and device identity tuples must be unique across the group, with case-sensitive comparison. A provided array replaces the complete list; on PATCH, omission or null preserves it and [] clears it. On create, omission or null supplies an empty list. |
| `site_id` | `str` | No | ID of the Site whose Expected Rack Groups should be replaced |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `topology` | `str` | No | Optional replacement topology identifier, non-blank and at most 128 characters. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Expected Rack Group
  nvidia.infra_controller.expected_rack_group:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-expected-rack-group"

- name: Delete a Expected Rack Group
  nvidia.infra_controller.expected_rack_group:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-expected-rack-group"
```
