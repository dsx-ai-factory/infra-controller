# nvidia.infra_controller.site_explorer_endpoint_info – query Site Explorer

Retrieve all Explored Endpoints

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `machine_id` | `str` | No | Only return endpoints whose exploration report has this Machine ID. Omit to return all endpoints in the Site. |
| `site_id` | `str` | Yes | ID of the Site |

## Examples

```yaml
- name: List all Site Explorer resources
  nvidia.infra_controller.site_explorer_endpoint_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"
```
