# nvidia.infra_controller.rack_info – query Rack

Retrieve all Racks

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `include_components` | `bool` | No | Include rack components in response |
| `manufacturer` | `str` | No | Filter by manufacturer |
| `name` | `str` | No | Filter by rack name |
| `site_id` | `str` | Yes | ID of the Site to retrieve Racks from |

## Examples

```yaml
- name: List all Rack resources
  nvidia.infra_controller.rack_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"

- name: Get a specific Rack by ID
  nvidia.infra_controller.rack_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"
    id: "{{ resource_id }}"
```
