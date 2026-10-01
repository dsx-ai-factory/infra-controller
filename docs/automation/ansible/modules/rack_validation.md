# nvidia.infra_controller.rack_validation – manage Rack

Manage Rack resources.

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource. When provided, targets a single resource. |
| `manufacturer` | `str` | No | Filter racks by manufacturer |
| `name` | `str` | No | Filter racks by name |
| `site_id` | `str` | Yes | ID of the Site |

## Examples

```yaml
- name: Run validation on all rack resources
  nvidia.infra_controller.rack_validation:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Run validation on a specific rack
  nvidia.infra_controller.rack_validation:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
