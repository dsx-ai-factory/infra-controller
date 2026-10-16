# nvidia.infra_controller.infiniband_partition_info – query InfiniBand Partition

Retrieve all InfiniBand Partitions

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `query` | `str` | No | Search for matches across all InfiniBand Partitions. Input will be matched against name, description, and status fields |
| `site_id` | `str` | No | Filter Partitions by Site |
| `status` | `str` | No | Filter Partitions by Status |

## Examples

```yaml
- name: List all InfiniBand Partition resources
  nvidia.infra_controller.infiniband_partition_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific InfiniBand Partition by ID
  nvidia.infra_controller.infiniband_partition_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
