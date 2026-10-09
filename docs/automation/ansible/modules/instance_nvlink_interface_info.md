# nvidia.infra_controller.instance_nvlink_interface_info – query Instance

Retrieve all Instance NVLink Interfaces

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `instance_id` | `str` | Yes | ID path parameter: instance_id. |
| `status` | `str` | No | Filter NVLink Interfaces by Status. Can be specified multiple times to filter on more than one status. |

## Examples

```yaml
- name: List all Instance resources
  nvidia.infra_controller.instance_nvlink_interface_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    instance_id: "{{ instance_id }}"
```
