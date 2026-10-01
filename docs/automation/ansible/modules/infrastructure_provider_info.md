# nvidia.infra_controller.infrastructure_provider_info – query Infrastructure Provider

Retrieve Infrastructure Provider for current Org

## Parameters

No additional parameters beyond the [authentication fragment](../getting-started.md).

## Examples

```yaml
- name: List all Infrastructure Provider resources
  nvidia.infra_controller.infrastructure_provider_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
```
