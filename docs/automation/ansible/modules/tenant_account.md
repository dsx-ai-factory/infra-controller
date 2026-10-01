# nvidia.infra_controller.tenant_account – manage Tenant Account

Retrieve all Tenant Accounts

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `infrastructure_provider_id` | `str` | No | Deprecated; inferred from the caller's org Infrastructure Provider when omitted. When provided, the value must match the org's Infrastructure Provider — mismatched values are rejected with 400. |
| `site_capabilities` | `list` | No | Provider Admin replace payload for TargetedInstanceCreation configuration. Required to be non-empty when sent. PATCH uses replace semantics: previously configured per-site overrides whose siteId is omitted from the new payload are cleared. Server validation rules: - must contain at least one entry - must contain exactly one entry with omitted or empty siteIds - must not repeat any siteId across entries - every provided siteId must be a valid Site UUID - every provided siteId must identify a Site associated with the Tenant and owned by the Tenant Account's Infrastructure Provider; otherwise the server rejects the request with 400 |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `tenant_contact_id` | `str` | No | Tenant Admin invite acceptance; must match the requesting user |
| `tenant_org` | `str` | No | Must be a valid Org name |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Tenant Account
  nvidia.infra_controller.tenant_account:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present

- name: Delete a Tenant Account
  nvidia.infra_controller.tenant_account:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    id: "{{ resource_id }}"
```
