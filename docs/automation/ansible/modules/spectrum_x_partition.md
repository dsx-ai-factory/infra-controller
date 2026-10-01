# nvidia.infra_controller.spectrum_x_partition – manage SpectrumX Partition

Retrieve all SpectrumX Partitions

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `description` | `str` | No | Optional description of the Partition |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `labels` | `dict` | No | String key-value pairs describing Partition labels. Up to 10 key-value pairs can be specified |
| `name` | `str` | No | Name of the Partition to create |
| `site_id` | `str` | No | ID of the Site the Partition should belong to |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `vni` | `int` | No | Requested VXLAN Network Identifier. Omit it to let the Site allocate one from its pool. A requested value that is already allocated or outside that pool is rejected by the Site |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a SpectrumX Partition
  nvidia.infra_controller.spectrum_x_partition:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-spectrum-x-partition"

- name: Delete a SpectrumX Partition
  nvidia.infra_controller.spectrum_x_partition:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-spectrum-x-partition"
```
