# nvidia.infra_controller.nvlink_logical_partition – manage NVLink Logical Partition

Create, update, and delete NVLink Logical Partitions.

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `description` | `str` | No | Optional description of the NVLink Logical Partition |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `name` | `str` | No | Name of the NVLink Logical Partition to create. Required when creating a new resource. |
| `site_id` | `str` | No | ID of the Site the NVLink Logical Partition should belong to. Required when creating a new resource. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a NVLink Logical Partition
  nvidia.infra_controller.nvlink_logical_partition:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-nvlink-logical-partition"
    site_id: "{{ site_id }}"

- name: Delete a NVLink Logical Partition
  nvidia.infra_controller.nvlink_logical_partition:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-nvlink-logical-partition"
```
