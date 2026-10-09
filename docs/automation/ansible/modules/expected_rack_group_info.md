# nvidia.infra_controller.expected_rack_group_info – query Expected Rack Group

Retrieve all Expected Rack Groups

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `site_id` | `str` | No | ID of the Site to filter Expected Rack Groups by |

## Examples

```yaml
- name: List all Expected Rack Group resources
  nvidia.infra_controller.expected_rack_group_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific Expected Rack Group by ID
  nvidia.infra_controller.expected_rack_group_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
