# nvidia.infra_controller.site_tenant_identity_token_delegation_info – query Tenant Identity

Retrieve Token Delegation for current Org

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `site_id` | `str` | Yes | ID path parameter: site_id. |

## Examples

```yaml
- name: List all Tenant Identity resources
  nvidia.infra_controller.site_tenant_identity_token_delegation_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"
```
