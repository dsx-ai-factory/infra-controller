# nvidia.infra_controller.infiniband_partition – manage InfiniBand Partition

Retrieve all InfiniBand Partitions

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `description` | `str` | No | Optional description of the Partition |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `labels` | `dict` | No | String key-value pairs describing Partition labels. Up to 10 key-value pairs can be specified |
| `name` | `str` | No | Name of the Partition to create |
| `site_id` | `str` | No | ID of the Site the Partition should belong to |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a InfiniBand Partition
  nvidia.infra_controller.infiniband_partition:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-infiniband-partition"

- name: Delete a InfiniBand Partition
  nvidia.infra_controller.infiniband_partition:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-infiniband-partition"
```
