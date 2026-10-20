# nvidia.infra_controller.instance_info – query Instance

Retrieve all Instances

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `infrastructure_provider_id` | `str` | No | Filter by Infrastructure Provider ID. Deprecated: Instances will no longer be filtered by Infrastructure Provider; results are scoped to the org's Tenant. Use the siteId parameter to scope results to a specific Infrastructure Provider's Sites. |
| `instance_type_id` | `str` | No | Filter by instance type ID. Can be specified multiple times to filter on more than one instance type. |
| `ip_address` | `str` | No | Filter by IP address. Can be specified multiple times to filter on more than one IP address. |
| `machine_id` | `str` | No | Filter by machine ID. Can be specified multiple times to filter on more than one machine. |
| `name` | `str` | No | Filter by Instance name |
| `network_security_group_id` | `str` | No | Filter by Network Security Group ID. Can be specified multiple times to filter on more than one Network Security Group. |
| `operating_system_id` | `str` | No | Filter by operating system ID. Can be specified multiple times to filter on more than one operating system. |
| `query` | `str` | No | Search for matches across all Instances. Input will be matched against name, description, status, and labels fields |
| `site_id` | `str` | No | Filter by Site ID. Can be specified multiple times to filter on more than one site. |
| `status` | `str` | No | Filter Instances by Status. Can be specified multiple times to filter on more than one status. |
| `vpc_id` | `str` | No | Filter by VPC ID. Can be specified multiple times to filter on more than one VPC. |

## Examples

```yaml
- name: List all Instance resources
  nvidia.infra_controller.instance_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific Instance by ID
  nvidia.infra_controller.instance_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
