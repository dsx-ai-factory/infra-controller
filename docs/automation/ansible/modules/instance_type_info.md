# nvidia.infra_controller.instance_type_info – query Instance Type

Retrieve all Instance Types

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `exclude_unallocated` | `bool` | No | Excludes Instance Type records that have no allocations from being returned in the result set. Currently can only be requested by Tenant. |
| `id` | `str` | No | ID of the resource to retrieve. |
| `include_allocation_stats` | `bool` | No | Include Allocation stats. |
| `include_machine_assignment` | `bool` | No | Include Machine assignments for each Instance Type. Can only be requested by Provider. |
| `infrastructure_provider_id` | `str` | No | Filter Instance Types by Infrastructure Provider ID. |
| `query` | `str` | No | Search for matches across all Instance Types. Input will be matched against name, display name, description, labels, and status fields |
| `site_id` | `str` | No | Filter Instance Types by Site ID |
| `status` | `str` | No | Filter Instance Types by Status |
| `tenant_id` | `str` | No | Filter Instance Types by Tenant ID. |

## Examples

```yaml
- name: List all Instance Type resources
  nvidia.infra_controller.instance_type_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific Instance Type by ID
  nvidia.infra_controller.instance_type_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
