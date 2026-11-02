# Organization & Permissions

NICo does not maintain its own user directory. Identity, org membership, and role assignments are all managed in the upstream identity provider. The REST API reads role claims from the authentication token on every request. Adding or removing a user is done in the identity provider, not through nicocli.

NICo accepts tokens from any OIDC-compatible IdP. The bundled dev Keycloak (deployed by `setup.sh` and documented in the [Quick Start Guide](../getting-started/quick-start.md)) is the recommended starting point and the reference implementation for IdP wiring. You can use it as-is for evaluation, or model your production IdP setup after it. Configure additional or replacement IdPs via the `issuers` block in `nico-rest-api`'s config; refer to [Authentication and Authorization](/rest-api-reference/authentication-and-authorization) for the configuration surface, the claim mappings NICo expects (org name, display name, role claim), and the validation rules that apply before rollout.

## Roles

NICo's authorization model has three roles, all managed in the upstream identity provider:

| Role | Scope | Required For |
|------|-------|-------------|
| Provider Admin (`PROVIDER_ADMIN`) | Infrastructure provider org | Creating allocations, managing tenant accounts, managing sites and instance types |
| Provider Viewer (`PROVIDER_VIEWER`) | Infrastructure provider org | Read-only access to provider-scoped resources |
| Tenant Admin (`TENANT_ADMIN`) | Tenant org | Managing the tenant's instances, VPCs, subnets, SSH keys |

A single user can hold roles in multiple orgs simultaneously. On dev/service-account orgs, one user typically holds both Provider Admin and Tenant Admin in the same org.

## Adding a User to a Tenant

1. Add the user to the IdP organization (or group) that maps to the tenant.
2. Assign the `TENANT_ADMIN` role at the org level.
3. Have the user authenticate with nicocli and verify: `nicocli user get`

The exact steps depend on your IdP. For Keycloak, NICo reads org membership from realm roles named `<orgName>:<ROLE>`, so "add the user to the org" means assigning that realm role, either directly or through a group whose role mapping includes it. [Tenant Management with Keycloak](tenant-management-keycloak.md) has the commands. See the [Quick Start Guide](../getting-started/quick-start.md) for the bundled realm layout.

## Adding a Provider Admin

1. Add the user to the infrastructure provider's IdP organization.
2. Assign the `PROVIDER_ADMIN` role.
3. Verify: `nicocli user get`

Same caveat as above, this is an IdP admin task, not a nicocli operation.

## Verifying Your Identity

```bash
nicocli user get
```

Example response for a human user:

```json
{
  "id": "<user-uuid>",
  "email": "alex@acme-corp.com",
  "firstName": "Alex",
  "lastName": "Chen",
  "created": "2026-04-23T00:54:27.452525Z",
  "updated": "2026-05-15T17:25:23.884166Z"
}
```

Service accounts have empty `email`/`firstName`/`lastName`. Human users have those populated from the IdP.

This endpoint does not return role information directly, roles are in the token and validated server-side. Confirm which roles you hold by attempting role-gated operations: `nicocli allocation list` requires Provider Admin; `nicocli tenant current` requires Tenant Admin.

If your deployment is configured for service-account auth, use `nicocli service-account current` to retrieve the current org's service-account status, including the auto-created provider and tenant IDs.

## Listing Tenant Members

NICo does not expose a "list users in this tenant" endpoint. Use the IdP's admin console or API to view members and roles.

For audit purposes, NICo's audit log records which user performed each operation:

```bash
nicocli tui
> audit list
```

## Removing a User

Remove the role assignment (or org membership) in the identity provider. The change takes effect on the user's next authentication attempt.

## Day One User Setup Checklist

1. Create the IdP organization (or group) for the tenant.
2. Invite at least one user with `TENANT_ADMIN` so they can provision the tenant.
3. Invite additional team members with appropriate roles.
4. Verify from the tenant side: `nicocli user get` and `nicocli tenant current`.
5. On the provider side, ensure at least one user holds `PROVIDER_ADMIN`.
