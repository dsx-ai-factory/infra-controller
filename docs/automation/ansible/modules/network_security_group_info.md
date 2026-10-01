# nvidia.infra_controller.network_security_group_info – query Network Security Group

Retrieve all Network Security Groups

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `include_attachment_stats` | `bool` | No | Include counts for the number objects that have attached the Network Security Group |
| `query` | `str` | No | Search for matches across all Network Security Groups. Input will be matched against name, description, and status fields |
| `site_id` | `str` | No | Filter By Site ID |
| `status` | `str` | No | Filter Network Security Groups by Status |

## Examples

```yaml
- name: List all Network Security Group resources
  nvidia.infra_controller.network_security_group_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific Network Security Group by ID
  nvidia.infra_controller.network_security_group_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
