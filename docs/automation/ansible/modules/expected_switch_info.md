# nvidia.infra_controller.expected_switch_info – query Expected Switch

Retrieve all Expected Switches

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `site_id` | `str` | No | ID of the Site to filter Expected Switches by |

## Examples

```yaml
- name: List all Expected Switch resources
  nvidia.infra_controller.expected_switch_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific Expected Switch by ID
  nvidia.infra_controller.expected_switch_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
