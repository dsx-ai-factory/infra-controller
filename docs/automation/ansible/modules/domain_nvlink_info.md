# nvidia.infra_controller.domain_nvlink_info – query Domain

Retrieve all NVLink Domains

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `include_components` | `bool` | No | Include component details in the response. |
| `name` | `list` | No | Filter by Domain name. Repeated values are ORed. |
| `site_id` | `str` | Yes | ID of the Site |

## Examples

```yaml
- name: List all Domain resources
  nvidia.infra_controller.domain_nvlink_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"

- name: Get a specific Domain by ID
  nvidia.infra_controller.domain_nvlink_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"
    id: "{{ resource_id }}"
```
