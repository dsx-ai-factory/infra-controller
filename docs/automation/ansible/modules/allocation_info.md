# nvidia.infra_controller.allocation_info – query Allocation

Retrieve all Allocations

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `constraint_type` | `str` | No | Filter Allocations by Constraint Type. Can be specified multiple times to filter on more than one Constraint Type. Choices: `Reserved`, `OnDemand`, `Preemptible`. |
| `constraint_value` | `int` | No | Filter Allocations by Constraint Value. Can be specified multiple times to filter on more than one Constraint Value. |
| `id` | `str` | No | Filter Allocations by ID. Can be specified multiple times to filter on more than one ID. |
| `infrastructure_provider_id` | `str` | No | Filter Allocations by Infrastructure Provider ID. |
| `query` | `str` | No | Search for matches across all Allocations. Input will be matched against name, description, and status fields |
| `resource_type` | `str` | No | Filter Allocations by Constraint Resource Type. Can be specified multiple times to filter on more than one Constraint Resource Type. Choices: `InstanceType`, `IPBlock`. |
| `resource_type_id` | `str` | No | Filter Allocations by Constraint Resource Type ID. Can be specified multiple times to filter on more than one Constraint Resource Type ID. |
| `site_id` | `str` | No | Filter Allocations by Site ID. Can be specified multiple times to filter on more than one Site ID. |
| `status` | `str` | No | Filter Allocations by Status. Can be specified multiple times to filter on more than one Status. |
| `tenant_id` | `str` | No | Filter Allocations by Tenant ID. |

## Examples

```yaml
- name: List all Allocation resources
  nvidia.infra_controller.allocation_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific Allocation by ID
  nvidia.infra_controller.allocation_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
