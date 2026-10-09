# nvidia.infra_controller.vpc_peering_info – query VPC Peering

Retrieve all VPC peerings

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `is_multi_tenant` | `bool` | No | Optional filter by peering tenancy type (single-tenant or multi-tenant). |
| `peer_tenant_id` | `str` | No | Optional filter by tenant ID of a VPC involved in the peering. Repeat the parameter to match multiple tenants. |
| `site_id` | `str` | No | Optional Site ID filter. If provided, caller must have access to the specified Site. |
| `status` | `str` | No | Optional filter by peering status. Repeat the parameter to match multiple statuses. Choices: `Pending`, `Configuring`, `Requested`, `Ready`, `Deleting`, `Error`. |
| `vpc_id` | `str` | No | Optional filter by VPC ID involved in the peering as either vpc1 or vpc2. Repeat the parameter to match multiple VPCs. |

## Examples

```yaml
- name: List all VPC Peering resources
  nvidia.infra_controller.vpc_peering_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific VPC Peering by ID
  nvidia.infra_controller.vpc_peering_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
