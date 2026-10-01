# nvidia.infra_controller.tenant_account_info – query Tenant Account

Retrieve all Tenant Accounts

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `infrastructure_provider_id` | `str` | No | Filter Tenant Accounts by Infrastructure Provider ID. Deprecated: Infrastructure Provider is now inferred from the org's membership. |
| `query` | `str` | No | Search string to filter Tenant Accounts by account number, tenant org, or tenant org display name |
| `tenant_id` | `str` | No | Filter Tenant Accounts by Tenant ID |

## Examples

```yaml
- name: List all Tenant Account resources
  nvidia.infra_controller.tenant_account_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific Tenant Account by ID
  nvidia.infra_controller.tenant_account_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
