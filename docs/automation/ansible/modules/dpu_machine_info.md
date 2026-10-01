# nvidia.infra_controller.dpu_machine_info – query DPU Machine

Retrieve all DPU Machines

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `site_id` | `str` | Yes | ID of the Site |

## Examples

```yaml
- name: List all DPU Machine resources
  nvidia.infra_controller.dpu_machine_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"

- name: Get a specific DPU Machine by ID
  nvidia.infra_controller.dpu_machine_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"
    id: "{{ resource_id }}"
```
