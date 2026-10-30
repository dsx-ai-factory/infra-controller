# nvidia.infra_controller.service_account_info – query Service Account

Retrieve Service Account status for current org

## Parameters

No additional parameters beyond the [authentication fragment](../getting-started.md).

## Examples

```yaml
- name: List all Service Account resources
  nvidia.infra_controller.service_account_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
```
