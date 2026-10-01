# nvidia.infra_controller.dpu_extension_service_info – query DPU Extension Service

Retrieve all DPU Extension Services

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `query` | `str` | No | Search for matches across all DPU Extension Services. Input will be matched against name, description, and status fields |
| `site_id` | `str` | No | Filter DPU Extension Services by Site ID |
| `status` | `str` | No | Status filter for the DPU Extension Services Choices: `Pending`, `Ready`, `Error`, `Deleting`. |

## Examples

```yaml
- name: List all DPU Extension Service resources
  nvidia.infra_controller.dpu_extension_service_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific DPU Extension Service by ID
  nvidia.infra_controller.dpu_extension_service_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
