# nvidia.infra_controller.site_info – query Site

Retrieve all Sites

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `include_gpu_stats` | `bool` | No | Include a per-Site breakdown of GPU counts grouped by GPU type. Requires Provider Admin role. |
| `include_machine_stats` | `bool` | No | Include a breakdown of Machine counts by lifecycle status and health. Requires Provider Admin role. |
| `infrastructure_provider_id` | `str` | No | Filter Sites by Infrastructure Provider ID. Deprecated: Infrastructure Provider is now inferred from the org's membership. |
| `is_flow_enabled` | `bool` | No | Filter Sites by NICo Flow enabled flag. Requires Provider Admin role. |
| `is_native_networking_enabled` | `bool` | No | Filter Sites by native networking enabled flag. Requires Provider Admin role. |
| `is_network_security_group_enabled` | `bool` | No | Filter Sites by network security group enabled flag. Requires Provider Admin role. |
| `is_nv_link_partition_enabled` | `bool` | No | Filter Sites by NVLink partitioning enabled flag. Requires Provider Admin role. |
| `query` | `str` | No | Search for matches across all Sites. Input will be matched against name, description, location, contact, and status fields |
| `status` | `str` | No | Filter Sites by Status. Can be specified multiple times to filter on more than one status |
| `tenant_id` | `str` | No | Filter Sites by Tenant ID. Deprecated: Tenant is now inferred from the org's membership. |

## Examples

```yaml
- name: List all Site resources
  nvidia.infra_controller.site_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific Site by ID
  nvidia.infra_controller.site_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
