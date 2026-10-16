# nvidia.infra_controller.site_tenant_identity_re_encrypt – manage Tenant Identity

Reencrypt Tenant Identity Secrets

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `dry_run` | `bool` | No | When true, decrypt and validate only; no changes are written. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `organization_id` | `str` | No | Optional tenant organization identifier (`org`), not the tenant's REST resource UUID or display name. A non-null value must contain one or more ASCII letters, digits, underscores, or hyphens; empty and whitespace-containing strings are rejected, not treated as site-wide scope. The value is matched case-insensitively and is lowercased before the Tenant lookup and before it reaches Core. The tenant must have an allocation and tenant identity configuration on the Site; only that organization's secrets are re-wrapped. The URL `{org}` separately identifies the provider authorizing the operation. If omitted or null, every row in the Site's tenant identity store is processed. |
| `site_id` | `str` | Yes | ID path parameter: site_id. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Tenant Identity
  nvidia.infra_controller.site_tenant_identity_re_encrypt:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    site_id: "{{ site_id }}"

```
