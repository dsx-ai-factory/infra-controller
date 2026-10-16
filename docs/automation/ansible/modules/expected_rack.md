# nvidia.infra_controller.expected_rack – manage Expected Rack

Retrieve all Expected Racks

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `description` | `str` | No | Human-readable description of the Expected Rack |
| `expected_racks` | `list` | No | The full desired set of Expected Racks for the Site. Every entry must reference the same `siteId` as the top-level field, and `rackId` values must be unique within the request. May be empty to clear all Expected Racks for the Site. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `labels` | `dict` | No | User-defined key-value pairs for organizing and categorizing Expected Racks. Well-known keys (`chassis.*`, `location.*`) are used to convey chassis identity and physical location. |
| `name` | `str` | No | Human-readable name of the Expected Rack |
| `rack_id` | `str` | No | Operator-supplied rack identifier. Immutable on update: omit this field, send `null`, or provide the existing value as a compatibility no-op. A changed value is rejected because Core and Flow use rackId as the identity key for expected racks. |
| `rack_profile_id` | `str` | No | Ignored compatibility field. Metadata updates preserve the stored profile; this field alone is not a valid update. |
| `site_id` | `str` | No | ID of the Site whose Expected Racks should be replaced |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Expected Rack
  nvidia.infra_controller.expected_rack:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-expected-rack"

- name: Delete a Expected Rack
  nvidia.infra_controller.expected_rack:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-expected-rack"
```
