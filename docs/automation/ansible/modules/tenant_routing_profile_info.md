# nvidia.infra_controller.tenant_routing_profile_info – query Tenant

Retrieve VPC routing profiles for current Tenant

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `site_id` | `str` | Yes | ID of the Site where the VPC will be created |

## Examples

```yaml
- name: List all Tenant resources
  nvidia.infra_controller.tenant_routing_profile_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"
```
