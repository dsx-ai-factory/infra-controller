# nvidia.infra_controller.rack_bringup_info – query Rack

Bring up Racks

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | Yes | ID path parameter: id. |

## Examples

```yaml
- name: List all Rack resources
  nvidia.infra_controller.rack_bringup_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific Rack by ID
  nvidia.infra_controller.rack_bringup_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
