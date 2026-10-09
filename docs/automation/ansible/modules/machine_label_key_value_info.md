# nvidia.infra_controller.machine_label_key_value_info – query Machine

Retrieve Machine label values

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `key` | `str` | Yes | ID path parameter: key. |
| `site_id` | `str` | No | ID of the Site to filter label values by |

## Examples

```yaml
- name: List all Machine resources
  nvidia.infra_controller.machine_label_key_value_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    key: "{{ key }}"
```
