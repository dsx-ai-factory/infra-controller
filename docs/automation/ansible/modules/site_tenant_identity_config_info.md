# nvidia.infra_controller.site_tenant_identity_config_info – query Tenant Identity

Retrieve Tenant Identity Configuration for current Org

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `site_id` | `str` | Yes | ID path parameter: site_id. |

## Examples

```yaml
- name: List all Tenant Identity resources
  nvidia.infra_controller.site_tenant_identity_config_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"
```
