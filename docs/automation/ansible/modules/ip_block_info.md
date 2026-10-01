# nvidia.infra_controller.ip_block_info – query IP Block

Retrieve all IP Blocks

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `include_usage_stats` | `bool` | No | Include IP Block usage stats in response |
| `infrastructure_provider_id` | `str` | No | Filter IP Blocks by Infrastructure Provider ID. Deprecated: Infrastructure Provider is now inferred from the org's membership. |
| `query` | `str` | No | Search for matches across all IP Blocks. Input will be matched against name, description, and status fields |
| `site_id` | `str` | No | Filter IP Blocks by Site ID |
| `status` | `str` | No | Filter IP Blocks by Status |
| `tenant_id` | `str` | No | Filter IP Blocks by Tenant ID. Deprecated: Tenant is now inferred from the org's membership. |

## Examples

```yaml
- name: List all IP Block resources
  nvidia.infra_controller.ip_block_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific IP Block by ID
  nvidia.infra_controller.ip_block_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
