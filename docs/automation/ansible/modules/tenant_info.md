# nvidia.infra_controller.tenant_info – query Tenant

Retrieve Tenant for current Org

## Parameters

No additional parameters beyond the [authentication fragment](../getting-started.md).

## Examples

```yaml
- name: List all Tenant resources
  nvidia.infra_controller.tenant_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
```
