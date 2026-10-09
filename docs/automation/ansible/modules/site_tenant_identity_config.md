# nvidia.infra_controller.site_tenant_identity_config – manage Tenant Identity

Retrieve Tenant Identity Configuration for current Org

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `allowed_audiences` | `list` | No | Allowlist of audience strings that may appear in issued JWT-SVIDs. When **empty or omitted**, the Core gRPC API persists `[defaultAudience]` as the stored allowlist, so only the default audience can be issued -- empty is **not** "allow any". To accept additional audiences, provide a non-empty list; non-empty lists must include `defaultAudience`. A subsequent GET returns the persisted allowlist, which may differ from an empty list that was sent on PUT. |
| `default_audience` | `str` | No | Default audience applied when a workload does not specify one. Required. |
| `enabled` | `bool` | No | Optional. Set to `true` to enable JWT-SVID issuance for this org or `false` to keep the config but pause issuance. Defaults to `true` when omitted. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `issuer` | `str` | No | JWT `iss` claim / OIDC issuer. Required in the REST request body. The Core gRPC API (`Issuer::parse`) is authoritative for scheme and host rules and accepts `https://`, `http://`, or `spiffe://` URLs with a DNS host; malformed values are rejected with `400 Bad Request`. |
| `rotate_key` | `bool` | No | Must be `true` for this variant. Generates a fresh ES256 signing keypair into the other key slot, swaps the current signer, and arms a JWKS overlap window for the previous key. |
| `signing_key_overlap_seconds` | `int` | No | Required when `rotateKey` is `true`. Number of seconds the previous verification key remains in JWKS so that JWTs already signed with it stay verifiable until they expire. Must be `>= tokenTtlSeconds`. The Core gRPC API enforces an upper bound via its `[machine_identity].signing_key_overlap_max_sec` config; values above that bound are rejected with `400 Bad Request`. |
| `site_id` | `str` | Yes | ID path parameter: site_id. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `subject_prefix` | `str` | No | Optional SPIFFE ID URI prefix for JWT `sub` (RFC-shaped `spiffe://…`). When omitted, the Core gRPC API derives a prefix from the issuer's trust domain. |
| `token_ttl_seconds` | `int` | No | Issued-token TTL in seconds. Required in the REST request body and must be > 0. The Core gRPC API enforces its configured `[machine_identity].token_ttl_min_sec` / `token_ttl_max_sec` window and rejects values outside the window with `400 Bad Request`. The window is per-site, so REST does not advertise a fixed maximum here. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Tenant Identity
  nvidia.infra_controller.site_tenant_identity_config:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    site_id: "{{ site_id }}"

- name: Delete a Tenant Identity
  nvidia.infra_controller.site_tenant_identity_config:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    id: "{{ resource_id }}"
    site_id: "{{ site_id }}"
```
