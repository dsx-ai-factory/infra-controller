# nvidia.infra_controller.expected_machine_info – query Expected Machine

Retrieve all Expected Machines

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `site_id` | `str` | No | ID of the Site to filter Expected Machines by |

## Examples

```yaml
- name: List all Expected Machine resources
  nvidia.infra_controller.expected_machine_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific Expected Machine by ID
  nvidia.infra_controller.expected_machine_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
