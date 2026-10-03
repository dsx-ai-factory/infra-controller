# nvidia.infra_controller.spectrum_x_partition_info – query SpectrumX Partition

Retrieve all SpectrumX Partitions

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `query` | `str` | No | Search for matches across all SpectrumX Partitions. Input will be matched against name, description, status and labels fields |
| `site_id` | `str` | No | Filter Partitions by Site. Repeat the parameter to match multiple Sites. |
| `status` | `str` | No | Filter Partitions by Status. Repeat the parameter to match multiple Statuses. Choices: `Pending`, `Provisioning`, `Ready`, `Error`, `Deleting`. |

## Examples

```yaml
- name: List all SpectrumX Partition resources
  nvidia.infra_controller.spectrum_x_partition_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific SpectrumX Partition by ID
  nvidia.infra_controller.spectrum_x_partition_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
