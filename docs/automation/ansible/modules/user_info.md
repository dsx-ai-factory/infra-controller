# nvidia.infra_controller.user_info – query User

Retrieve Current User

## Parameters

No additional parameters beyond the [authentication fragment](../getting-started.md).

## Examples

```yaml
- name: List all User resources
  nvidia.infra_controller.user_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
```
