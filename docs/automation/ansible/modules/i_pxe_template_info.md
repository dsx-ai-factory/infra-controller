# nvidia.infra_controller.i_pxe_template_info – query iPXE Template

Get all iPXE templates

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `site_id` | `list` | No | Optional site ID(s); may be repeated to restrict results to templates available at any of the sites |

## Examples

```yaml
- name: List all iPXE Template resources
  nvidia.infra_controller.i_pxe_template_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific iPXE Template by ID
  nvidia.infra_controller.i_pxe_template_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
