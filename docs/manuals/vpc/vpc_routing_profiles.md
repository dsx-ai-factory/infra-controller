# VPC Routing Profiles

This page describes how to create VPCs based on the routing profile configuration of the site. Routing profile configuration is part of the required baseline server configuration for successful VPC creation.

This page is intended for engineers who are responsible for configuring or operating a production API server.

## Core Concepts

### VPC

A VPC is the logical network container used for tenant workloads. It defines the tenant boundary for networking behavior and provides the parent context for related resources such as prefixes and segments.

### Network Virtualization Type

A VPC has a `network_virtualization_type` that determines how the platform implements networking for that VPC. There are two supported values:

- `FNN`: The production networking model
- `ETHERNET_VIRTUALIZER`: A legacy, deprecated, and not officially supported model. It may still appear in existing objects or older workflows, but it should not be treated as the target model for production planning.

> **Important**: The default applied when no virtualization type is supplied at VPC creation depends on the site and the interface used. On a site with native networking enabled — the default for all newly created sites — the REST API and `nicocli` default an unspecified type to `FNN`. On a site without native networking (a legacy site, or one explicitly downgraded), and when a VPC is created directly through the core gRPC API, the default remains `ETHERNET_VIRTUALIZER`. Regardless of the default, always specify the `FNN` option explicitly for VPCs on a production site so the intended model is never left to inference.

### Routing Profile Type

A VPC also has a `routing_profile_type`, which determines the routing policy class associated with that VPC.

Routing profile names are free-form strings, not a fixed enumeration. The names `EXTERNAL`, `INTERNAL`, `MAINTENANCE`, and `PRIVILEGED_INTERNAL` are conventional and commonly used, but they are not reserved or hardcoded by the API. Any name is accepted, provided it is defined as a key in `fnn.routing_profiles` in the API server configuration.

The API validates all supplied profile names against the configured profiles. Supplying an unknown name returns a `NOT_FOUND` error.

The resolved `routing_profile_type` is returned on the `Vpc` resource in API responses, reflecting the profile selected at creation time, whether supplied explicitly in the creation request or inherited from the tenant.

> **REST API note**: The REST API reserves three profile names for VPC creation: `external`, `internal`, and `privileged-internal`. These are the only values the REST API accepts for the `routingProfile` field; any other value is rejected. They are translated to their uppercase equivalents (`EXTERNAL`, `INTERNAL`, `PRIVILEGED_INTERNAL`) before being forwarded to the API server. Sites that serve REST API clients must define all three reserved names in `fnn.routing_profiles`. Additional profiles may be defined and used through the gRPC interface.

### API Server Routing Profiles

The API server must define the available routing profiles under the `fnn.routing_profiles` section of the configuration file.

Each entry is keyed by the routing profile name and contains the site-specific routing behavior associated with that profile. This includes whether the profile is treated as internal or external, which route-policy settings apply, and the profile's `access_tier` value.

The `access_tier` field governs privilege enforcement between tenants and VPCs. Lower values represent broader (less-restricted) access; higher values represent narrower (more-restricted) access. A VPC cannot be created with a profile whose `access_tier` is lower than the tenant's profile `access_tier`. Every profile intended for production use must have an `access_tier` assigned.

The `default_tenant_routing_profile_type` field in the top-level server configuration controls which profile is assigned to new tenants when FNN is enabled and no profile is supplied at creation time. It defaults to `EXTERNAL` if not set:

```toml
default_tenant_routing_profile_type = "EXTERNAL"
```

## Relationship between network_virtualization_type and routing_profile_type

The `network_virtualization_type` and `routing_profile_type` settings are related, but they serve different purposes.

- The `network_virtualization_type` determines how the VPC is implemented (i.e. it selects the networking model).
- The `routing_profile_type` determines which routing policy the VPC uses.
- The API server `fnn.routing_profiles` configuration defines what each routing profile means at that site.

## How the API Selects a VPC Routing Profile

When a VPC is created, the API determines the routing profile as follows:

1. If the create request includes `routing_profile_type`, that value is used.
2. If the request does not include `routing_profile_type`, the API uses the tenant’s `routing_profile_type`.
3. The API then looks for a routing profile with the same name in `fnn.routing_profiles`.

The API also enforces access boundaries using the `access_tier` values defined in `fnn.routing_profiles`. A VPC cannot request a routing profile whose `access_tier` is lower than the tenant’s profile `access_tier`. Lower `access_tier` values represent broader access; higher values represent narrower access. For example, if `EXTERNAL` has `access_tier = 2` and `INTERNAL` has `access_tier = 1`, a tenant assigned `EXTERNAL` cannot create a VPC with profile `INTERNAL` because `1 < 2`. A tenant assigned `INTERNAL` can, however, create a VPC with profile `EXTERNAL`.

## Why Routing Profile Configuration Is Required in Production

Routing profile resolution is part of standard production-site VPC creation. The API uses the selected routing profile during VPC setup, including VNI allocation behavior. As a result, a production site must define the routing profiles that tenants and VPCs are expected to use.

Even if a site has legacy objects that use `ETHERNET_VIRTUALIZER`, production operations should still be planned around the `FNN` routing-profile model. The presence of the legacy virtualization type does not remove the need for correct FNN routing profile configuration.

## Required API Server Configuration

At a minimum, the API server should define every routing profile type that may be assigned to a tenant or used by a VPC.

A representative TOML example is shown below:

```toml
[fnn]

[fnn.routing_profiles.EXTERNAL]
internal = false
access_tier = 2
route_target_imports = []
route_targets_on_exports = []
leak_default_route_from_underlay = false
leak_tenant_host_routes_to_underlay = false

[fnn.routing_profiles.INTERNAL]
internal = true
access_tier = 1
route_target_imports = []
route_targets_on_exports = []
leak_default_route_from_underlay = false
leak_tenant_host_routes_to_underlay = false
```

If the site needs to support additional routing profile types, they should also be defined explicitly:

```toml
[fnn]

[fnn.routing_profiles.EXTERNAL]
internal = false
access_tier = 3
route_target_imports = []
route_targets_on_exports = []
leak_default_route_from_underlay = false
leak_tenant_host_routes_to_underlay = false

[fnn.routing_profiles.INTERNAL]
internal = true
access_tier = 2
route_target_imports = []
route_targets_on_exports = []
leak_default_route_from_underlay = false
leak_tenant_host_routes_to_underlay = false

[fnn.routing_profiles.MAINTENANCE]
internal = true
access_tier = 1
route_target_imports = []
route_targets_on_exports = []
leak_default_route_from_underlay = false
leak_tenant_host_routes_to_underlay = false

[fnn.routing_profiles.PRIVILEGED_INTERNAL]
internal = true
access_tier = 0
route_target_imports = []
route_targets_on_exports = []
leak_default_route_from_underlay = false
leak_tenant_host_routes_to_underlay = false
```

The exact route-target values, leak settings, and `access_tier` values are site-specific, but the profile names must exist and must match the API values exactly. The `access_tier` values must be assigned consistently across all profiles to reflect the intended access hierarchy.

## How Tenant Routing Profiles Affect VPC Creation

Each tenant may have a `routing_profile_type`. In a production site, this serves as the default routing profile for VPCs created under that tenant. This has the following important consequences:

- If a VPC creation request does not specify `routing_profile_type`, the tenant's routing profile is used automatically.
- If the tenant is configured with a profile that is not present in `fnn.routing_profiles`, VPC creation will fail.
- An FNN VPC creation request must reference an existing tenant with a routing profile.

For this reason, tenant configuration and API server routing profile configuration must be managed together.

## Changing a Tenant’s Routing Profile

A tenant's routing profile can only be changed if *the tenant has no active FNN VPCs*. Otherwise, the API server rejects the update.

When FNN is enabled, `UpdateTenant` requests must include `routing_profile_type`. Clients updating only tenant metadata must preserve and resend the tenant's current profile.

This restriction exists because FNN VPC behavior depends on the tenant's permitted routing profile, and changing the tenant's profile while FNN VPCs already exist could invalidate assumptions made when those VPCs were created. Non-FNN VPCs do not consume tenant routing policy, so they do not block a profile change.

### Process for Changing a Tenant's Routing Profile

The following is a safe operational sequence for changing a tenant's routing profile:

1. Confirm that the destination routing profile is already defined in `fnn.routing_profiles` on the API server.
2. Verify that the tenant has no active FNN VPCs.
3. Update the tenant's `routing_profile_type`.
4. Create new VPCs for that tenant using the updated profile policy.

If the tenant has active FNN VPCs, those VPCs must be deleted before the tenant profile can be changed. Non-FNN VPCs may remain active.

### Using the admin-cli

When FNN is enabled, the API server assigns newly created tenants a default routing profile. This default is controlled by the `default_tenant_routing_profile_type` field in the API server configuration, and defaults to `EXTERNAL` if not set.

For deployments where this is insufficient, the gRPC admin-cli supports tenant profile updates through the `tenant update` command.

The tenant organization ID is required as a positional argument:

```
admin-cli tenant update <tenant-org> -p <profile>
```

**Examples**

```
admin-cli tenant update example-org -p EXTERNAL
admin-cli tenant update example-org -p INTERNAL
admin-cli tenant update example-org -p PRIVILEGED_INTERNAL
admin-cli tenant update example-org -p MAINTENANCE
```

The `-p` flag accepts any string. The supplied value must exactly match a key defined in `fnn.routing_profiles` in the API server configuration. The API validates the name and returns `NOT_FOUND` if the profile does not exist in the configuration.

This is the recommended workflow for changing a tenant's routing profile using the admin-cli:

1. Review the current tenant record:

   `admin-cli tenant show <tenant-org>`

2. Confirm that the tenant has no active FNN VPCs.

3. Apply the update:

   ```
   admin-cli tenant update <tenant-org> -p INTERNAL
   ```

The CLI also supports an optional version-match flag:

```
admin-cli tenant update <tenant-org> -p INTERNAL -v <current-version>
```

This flag is optional. It is not a verbosity setting, but is used for optimistic concurrency checking and causes the update to be rejected if the tenant record has changed since it was last reviewed.

If the tenant still has active FNN VPCs, the command will fail. In this case, the existing FNN VPCs must be removed before the tenant routing profile can be changed.

### Operational implication

This means the tenant routing profile should be treated as a planning decision rather than a casual runtime toggle. It is possible to change while non-FNN VPCs remain active, but only when the tenant has no active FNN VPCs.

## Troubleshooting Example: External Routing Profile Not Found

Consider the following example error returned during VPC creation:

```
RoutingProfile not found: EXTERNAL
```

This error should be interpreted as a routing profile lookup failure during VPC creation.

### What This Means

The API determined that the effective routing profile type of the VPC was `EXTERNAL`. It then attempted to look up a routing profile named `EXTERNAL` in the `fnn.routing_profiles` configuration for the API server. That lookup failed because no matching entry was defined.

### Why This Happens

This commonly occurs in the following situations:

- The tenant's routing profile type is `EXTERNAL`, and the VPC request did not override it.
- The VPC request explicitly requested `EXTERNAL`.
- The API server configuration does not contain `[fnn.routing_profiles.EXTERNAL]`.
- The configuration contains a similar profile, but the key name does not exactly match `EXTERNAL`.

### How to Resolve This Issue

The appropriate resolution is to add the missing routing profile definition to the API server configuration and ensure that the tenant and VPC are using a profile that is intentionally supported by the site.

A minimal TOML example is shown below:

```toml
[fnn]

[fnn.routing_profiles.EXTERNAL]
internal = false
access_tier = 2
route_target_imports = []
route_targets_on_exports = []
leak_default_route_from_underlay = false
leak_tenant_host_routes_to_underlay = false
```

After adding the profile, also verify the following:

1. The tenant exists.
2. The tenant's `routing_profile_type` is the one you intend to use.
3. The VPC request is either inheriting the correct tenant profile or explicitly requesting the correct profile.
4. The profile name in the configuration exactly matches the API value.

### Broader Lessons

This example illustrates an important operational rule: In a production site, all routing profile types that may be assigned to tenants or requested by VPCs must already be defined in the API server configuration.

### Additional Troubleshooting Checklist

When investigating VPC creation failures related to routing profiles, the following checks are recommended:

1. Confirm that `FNN` is enabled on the site.
2. Confirm that the required routing profile exists under `fnn.routing_profiles`.
3. Confirm that the profile name is spelled exactly as expected.
4. Check the tenant’s `routing_profile_type`.
5. Check whether the VPC request explicitly supplied the `routing_profile_type`.
6. Confirm that the requested or inherited routing profile is permitted for that tenant.
7. Confirm that the routing profile definitions needed by the site are present before creating or updating tenants and VPCs.

## Troubleshooting Example: No Internet Access in a VPC

A tenant reports that instances in their VPC have no external connectivity.

### What This Means

Instances reach destinations outside the overlay by following a default route present in the VPC’s overlay routing table. If no default route exists in the overlay, outbound traffic to external destinations is dropped.

### Why This Happens

There are several common causes, and distinguishing between them requires comparing the routing profile against the expected network deployment:

* **The routing profile does not import the correct route-target.**

  The VPC’s routing profile may not include a `route_target_imports` entry that causes a default route to be imported into the overlay. Without such an import, the VPC has no default route regardless of what the network advertises.

* **The routing profile is correct, but the network injection is not occurring.**

  Some deployment models intentionally omit a default-route route-target import from the profile and instead rely on the network to inject a default route by advertising a route that matches the VPC’s native route-target (`<ASN>:<VNI_OF_VPC>`). In this case the profile is configured as intended, but the expected network-side advertisement is absent or misconfigured.

* **The network device VRF is not importing the VPC’s route-targets.**

  Even when the VPC has a default route and can forward traffic outbound, the network device’s VRF may not be configured to import the route-targets present on VPC routes. If the VRF does not import those route-targets, the network has no visibility into VPC prefixes and cannot return traffic to instances. This produces the same symptom—no external connectivity—despite the overlay routing table appearing correct from the VPC side.

### How to Resolve This Issue

Contact the team that manages the network infrastructure. Provide them with the routing profile assigned to the VPC, specifically the `route_target_imports` values and the `route_targets_on_exports` values, and have them compare those profiles against the expected network deployment. These are the questions to answer:

- Is this profile expected to import a specific route-target that carries a default route? If so, is that entry present and correct?
- Is this deployment relying on network injection via the VPC's native route-target? If so, is the network advertising the expected route?
- Is the network device VRF configured to import the route-targets present on routes exported by this VPC?

The resolution depends on the outcome of that comparison and lies with the network team, not with the API server routing profile configuration alone.
