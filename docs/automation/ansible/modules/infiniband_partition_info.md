# nvidia.infra_controller.infiniband_partition_info – query InfiniBand Partition

Retrieve all InfiniBand Partitions

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `infiniband_partition_id` | `str` | No | Filter InfiniBand Interfaces by InfiniBand Partition ID. Can be specified multiple times to filter on more than one ID. |
| `instance_id` | `str` | No | Filter InfiniBand Interfaces by Instance ID. Can be specified multiple times to filter on more than one ID. |
| `query` | `str` | No | Search for matches across all InfiniBand Partitions. Input will be matched against name, description, and status fields |
| `site_id` | `str` | No | Filter InfiniBand Interfaces by Site ID. Can be specified multiple times to filter on more than one ID. |
| `status` | `str` | No | Filter InfiniBand Interfaces by Status. Can be specified multiple times to filter on more than one status. |

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
