# nvidia.infra_controller.rack_health_report_info – query Rack

Retrieve all Rack health reports

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | Yes | ID of the resource to retrieve. |
| `site_id` | `str` | Yes | ID of the Site that owns the Rack |

## Examples

```yaml
- name: List all Rack resources
  nvidia.infra_controller.rack_health_report_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"

- name: Get a specific Rack by ID
  nvidia.infra_controller.rack_health_report_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"
    id: "{{ resource_id }}"
```
