# nvidia.infra_controller.site_tenant_identity_token_delegation – manage Tenant Identity

Retrieve Token Delegation for current Org

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `client_secret_basic` | `dict` | No | Client-secret basic authentication settings for token delegation |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `site_id` | `str` | Yes | ID path parameter: site_id. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `subject_token_audience` | `str` | No | Audience value placed on the intermediate JWT-SVID posted to the exchange endpoint. |
| `token_endpoint` | `str` | No | URL of the tenant's RFC 8693 token exchange endpoint. The Core gRPC API validates scheme and host against its configured `[machine_identity].token_endpoint_domain_allowlist` and rejects mismatches with `400 Bad Request`. Operators that need to enforce HTTPS-only must populate that allowlist. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Tenant Identity
  nvidia.infra_controller.site_tenant_identity_token_delegation:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    site_id: "{{ site_id }}"

- name: Delete a Tenant Identity
  nvidia.infra_controller.site_tenant_identity_token_delegation:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    id: "{{ resource_id }}"
    site_id: "{{ site_id }}"
```
