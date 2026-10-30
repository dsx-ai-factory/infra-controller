# nvidia.infra_controller.nvlink_logical_partition_info – query NVLink Logical Partition

Retrieve all NVLink Logical Partitions

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `include_interfaces` | `bool` | No | Include NVLink Interfaces in response. |
| `include_stats` | `bool` | No | Include NVLink Logical Partition Stats in response. |
| `include_vpcs` | `bool` | No | Include VPCs in response. |
| `query` | `str` | No | Search for matches across all NVLink Logical Partitions. Input will be matched against name, description, and status fields |
| `site_id` | `str` | No | Filter NVLink Logical Partitions by Site |
| `status` | `str` | No | Filter NVLink Logical Partitions by Status |

## Examples

```yaml
- name: List all NVLink Logical Partition resources
  nvidia.infra_controller.nvlink_logical_partition_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific NVLink Logical Partition by ID
  nvidia.infra_controller.nvlink_logical_partition_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
