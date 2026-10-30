# nvidia.infra_controller.expected_rack_info – query Expected Rack

Retrieve all Expected Racks

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `site_id` | `str` | No | ID of the Site to filter Expected Racks by |

## Examples

```yaml
- name: List all Expected Rack resources
  nvidia.infra_controller.expected_rack_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific Expected Rack by ID
  nvidia.infra_controller.expected_rack_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
