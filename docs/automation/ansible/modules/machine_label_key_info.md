# nvidia.infra_controller.machine_label_key_info – query Machine

Retrieve Machine label keys

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `site_id` | `str` | No | ID of the Site to filter label keys by |

## Examples

```yaml
- name: List all Machine resources
  nvidia.infra_controller.machine_label_key_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
```
