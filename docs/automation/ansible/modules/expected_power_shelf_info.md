# nvidia.infra_controller.expected_power_shelf_info – query Expected Power Shelf

Retrieve all Expected Power Shelves

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `site_id` | `str` | No | ID of the Site to filter Expected Power Shelves by |

## Examples

```yaml
- name: List all Expected Power Shelf resources
  nvidia.infra_controller.expected_power_shelf_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific Expected Power Shelf by ID
  nvidia.infra_controller.expected_power_shelf_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
