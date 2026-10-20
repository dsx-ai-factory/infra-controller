# nvidia.infra_controller.vpc_info – query VPC

Retrieve all VPCs

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `network_security_group_id` | `str` | No | Filter VPCs by Network Security Group ID. Can be specified multiple times to filter on more than one Network Security Group. |
| `nv_link_logical_partition_id` | `str` | No | Filter VPCs by NVLink Logical Partition ID. Can be specified multiple times to filter on more than one NVLink Logical Partition. |
| `query` | `str` | No | Search for matches across all VPCs. Input will be matched against name, description, labels, and status fields |
| `site_id` | `str` | No | Filter VPCs by Site ID. Can be specified multiple times to filter on more than one Site. |
| `status` | `str` | No | Filter VPCs by Status. Can be specified multiple times to filter on more than one Status. |

## Examples

```yaml
- name: List all VPC resources
  nvidia.infra_controller.vpc_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific VPC by ID
  nvidia.infra_controller.vpc_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
