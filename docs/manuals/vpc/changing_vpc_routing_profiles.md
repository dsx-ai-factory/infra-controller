# Changing a VPC Routing Profile

You can change an existing FNN VPC between configured internal and external routing profiles without recreating the VPC or its instances. Core changes the profile and active Virtual Network Identifier (VNI) in one transaction and retains the previous VNI for possible reversal.

This procedure is for site operators and infrastructure provider administrators. The change preserves VPC identity, instance addresses, prefixes, network security groups (NSGs), metadata, and the creation-time requested VNI. It does not renumber a guest or require a DHCP renewal.

<Warning>
A successful API response confirms the Core transaction, not restored traffic. DPUs apply the change asynchronously, and traffic can be interrupted. Keep the previous VNI allocated until every affected consumer has stopped using it. Releasing it early can let another VPC allocate a VNI that an old consumer still uses.
</Warning>

## Prepare the Site

Complete these checks before scheduling the change:

- Confirm the VPC uses `FNN`, has a named source profile, and the site does not configure `site_global_vpc_vni`.
- Confirm the source and destination exist in `fnn.routing_profiles` with opposite `internal` settings. The profile names are configuration-defined strings, not an enum. Changing to the same profile or another profile with the same `internal` setting is unsupported.
- Check the tenant's permitted access tier. The destination's `access_tier` must be greater than or equal to the tenant profile's tier. Lower values grant broader access. Provider authorization and retained ownership do not bypass this check.
- Check for unsupported attachments: substantive VPC routing overrides, tenant-managed `SitePrefix` attachments, or interface routing overrides with nonempty `allowed_anycast_prefixes`. Include deleting instances and both sides of pending interface changes.
- Verify that `vpc-vni` and `external-vpc-vni` contain disjoint materialized ranges. The current and destination VNIs must be between 1 and 16,777,215. Coordinate destination VNI and route-target coverage with the network team.
- Reserve destination capacity for this change, ordinary VPC creation, and outstanding retained allocations. During a transition, one VPC consumes an allocation in each pool. If the destination already retains this VPC's VNI, Core reuses it.

Refer to [VPC Routing Profiles](vpc_routing_profiles.md) for site policy and [VNI Resource Pools](vni_resource_pools.md) for capacity and pool configuration.

Core also applies other enabled site policy checks, including tenant-prefix overlap checks when a change expands routing access.

Verify the actual running builds and configuration on every participating Core replica and DPU. Core must support routing-state inspection, profile changes, inactive-VNI release, and exact VNI selection if you request it. An older Core can ignore an unknown exact-VNI field and allocate a different value; client response validation cannot undo that commit.

The DPU agent must detect changes to its own VNI and imported peer VNIs even when managed-host and instance network-config versions do not change. Agents that compare the complete rendered input or reconcile unconditionally can satisfy this requirement. Verify the deployed implementation, not only its version label. Different version numbers are not automatically incompatible; unsupported caching behavior or inconsistent routing-profile definitions prevent cutover.

Agree on an operational hold covering VPC deletion, instance attachments, interface configuration, peerings, and routing/profile definitions through verification and release. Core blocks VPC deletion while a retained allocation exists, but neither Core nor REST enforces the complete hold.

## Record the Starting State

Use the authoritative routing-state interface, not ordinary REST VPC inventory, which can lag Core. Record the VPC ID, active profile and VNI, retained allocation if present, and the exact Core version.

For CLI access, configure site credentials as described in [NICo Admin CLI](../nico-admin-cli.md). The following example saves a Core observation as JSON:

```bash
nico-admin-cli --format json --output ./routing-before.json \
  vpc routing-state 12345678-1234-5678-90ab-cdef01234567
```

For REST access, use the infrastructure provider organization and the REST VPC ID, which can differ from its Core ID. The provider must own both the VPC and its registered site. Your authorization role must have the `PROVIDER_ADMIN` suffix; tenant administrator access alone is insufficient.

The following request reads the same authoritative state through REST. Replace the example host, organization, ID, and bearer token with your deployment's values:

```http
GET /v2/org/example-provider/nico/vpc/12345678-1234-5678-90ab-cdef01234567/routing-profile HTTP/1.1
Host: api.example.com
Authorization: Bearer <access-token>
```

The REST response includes `version`, `routingProfile`, `activeVni`, and `retainedAllocation`. A null retained allocation means no inactive allocation exists; inconsistent ownership returns an error. The version is a Core precondition, not a DPU-applied generation. Inspection alone does not establish that the VPC supports a profile change or expose a site-global VNI override.

Keep a list of every DPU attached to this VPC and every directly peered VPC that imports its native route target. Include consumers with pending or deleting attachments. Capture the applied HBN/NVUE/FRR configuration, route-target imports and exports, EVPN routes, and expected BGP neighbors for each consumer. Ask the network team to capture the corresponding gateway and fabric routes.

Keep this original consumer list until cleanup finishes, and compare it with a fresh inventory before release. Removing a peering from the database does not prove its former DPU stopped importing the old route target. A VPC with zero instances can still have populated peers that need verification.

Choose traffic probes with sources and destinations that remain reachable under the destination profile. Record timestamps before and during the change. An external profile does not assign public IP addresses, configure NAT, install fabric routes, or change NSGs. Loss of internal access alone does not prove permitted external traffic works. Evaluate retained peerings separately when checking the intended access boundary.

## Change the Profile

Choose either the CLI or REST interface for the mutation. In the examples, `EXTERNAL` is a configured external profile; use your site's destination name. No separate resource-pool mutation is required.

### Using the CLI

For an interactive change, run the following command and review the displayed state and proposed action:

```bash
nico-admin-cli --cloud-unsafe-op admin vpc change-routing-profile \
  12345678-1234-5678-90ab-cdef01234567 EXTERNAL
```

The CLI reads the current Core version and asks for confirmation before submitting that frozen version. Both standard input and standard error must be terminals. Enter `yes` only after completing the preparation and starting-state checks. The `admin` value identifies the operator acknowledging the unsafe operation; it does not replace site credentials or authorization.

For scripts, supply the original version from your authoritative observation. The following example also requests an exact VNI:

```bash
nico-admin-cli --cloud-unsafe-op admin vpc change-routing-profile \
  12345678-1234-5678-90ab-cdef01234567 EXTERNAL \
  --if-version-match V1-T1789080000000000 --vni 51000
```

Replace the example version and VNI with your observed version and approved destination. Omit `--vni` for automatic selection. If the destination pool already retains a VNI for this VPC, an exact request must match it. Otherwise, the exact value must be a free materialized entry in that pool; entries with either `auto_assign` setting are eligible. Core rejects mismatched or unavailable exact values without falling back.

The CLI prints the pre-mutation observation as JSON to standard error and returns the authoritative result on success. Use `--format ascii-table` (the default), `json`, or `yaml`, and optional `--output PATH`, before `vpc`. CSV is unsupported for these operations.

### Using REST

Submit the destination profile and optional exact VNI in one PATCH request:

```http
PATCH /v2/org/example-provider/nico/vpc/12345678-1234-5678-90ab-cdef01234567/routing-profile HTTP/1.1
Host: api.example.com
Authorization: Bearer <access-token>
Content-Type: application/json

{
  "routingProfile": "external",
  "vni": 51000
}
```

REST reads Core's version internally and submits one version-guarded change. Do not supply a version or update resource pools separately. Omit `vni`, or set it to null, for automatic selection or retained-VNI reuse.

The destination name must contain 1 to 64 characters. REST translates `external`, `internal`, and `privileged-internal` to `EXTERNAL`, `INTERNAL`, and `PRIVILEGED_INTERNAL`; other configured names pass through unchanged. Responses translate those known names back to REST aliases.

HTTP 200 returns the committed profile, active VNI, advanced version, and retained previous allocation. HTTP 412 reports a stale version or failed Core precondition, including unsupported configuration or an unavailable exact VNI. Automatic pool exhaustion returns HTTP 429. REST does not reread a newer version and retry a rejected mutation.

## Verify DPU and Fabric Convergence

Compare the new Core observation with the recorded starting state. Confirm that the destination profile and intended VNI are active and that the previous VNI remains retained. Keep the operational hold while performing these checks for every original and newly discovered consumer:

1. Check the DPU agent's configuration-fetch and apply logs for the change and subsequent failures. A fetch or generic healthy status does not establish that HBN applied it.
2. Inspect the applied HBN/NVUE configuration and FRR running configuration. On target DPUs, verify the destination VNI and profile-specific imports, exports, and route-leak settings. On directly peered DPUs, verify replacement of the old peer-VNI import with the new one.
3. Inspect EVPN routes and the native route targets formed from the site's ASN and VNI. Confirm the old VNI and route-target references are withdrawn from every affected DPU and the relevant gateway/fabric state.
4. Compare expected BGP neighbor state and routing tables with the baseline. VPC peering adds route-target imports; it does not create a separate BGP session for each VPC pair. Established underlay BGP sessions alone do not prove correct VPC peering or forwarding.
5. Run the agreed traffic probes and verify both permitted connectivity and the intended access restrictions. Confirm addressing, NAT, and NSG prerequisites independently where they are required for the probe.

Use the access method and diagnostic commands supported by the deployed HBN version. Inspect both applied configuration and operational routing state; a desired configuration file alone is insufficient. In startup-file mode, `hbn.skip_reload` can save configuration without applying it. Check the applied NVUE revision and live FRR configuration, not only `/var/support/nvue_startup.yaml`. The agent's reported network-config version, general health, and an elapsed waiting period cannot prove this VPC-derived change converged.

Record the Core request/response times, each DPU's observed apply time, route-convergence observations, and probe results. Report the traffic disruption interval separately from configuration convergence. If no valid traffic probe ran, report only the observed configuration interval; do not describe it as measured tenant downtime.

If a consumer is unreachable or cannot be verified, retain the old allocation unless you can prove that consumer is isolated. Do not release based only on a timeout or an empty target instance count.

## Reverse the Change If Needed

To reverse, request the original profile using the same change operation. Core revalidates the current tenant authorization, attachments, profile definitions, and allocations. If accepted, it reuses the retained VNI and retains the VNI you are leaving.

For example, the following interactive command requests a return to a configured internal profile:

```bash
nico-admin-cli --cloud-unsafe-op admin vpc change-routing-profile \
  12345678-1234-5678-90ab-cdef01234567 INTERNAL
```

Through REST, PATCH the same routing-profile endpoint with `{"routingProfile":"internal"}`. Omit the exact VNI to reuse the retained destination, or provide that exact retained value. Reversal can interrupt traffic again and requires the same consumer and fabric verification.

Retention preserves the VNI number, not a historical profile definition or guaranteed permission to reverse. Keep the recorded original profile name and configuration. After release, the old VNI can be allocated to another VPC and is no longer reserved for reversal.

## Release the Inactive VNI

Release only after verifying convergence, retaining the operational hold, and deciding that you no longer need the reservation. Read authoritative state again and tie the release decision to that exact version and inactive VNI. If the state differs from what you verified, investigate before proceeding.

The following CLI example releases a verified inactive allocation:

```bash
nico-admin-cli --cloud-unsafe-op admin vpc release-inactive-vni \
  12345678-1234-5678-90ab-cdef01234567 \
  --if-version-match V2-T1789147200000000 \
  --expected-inactive-vni 12001 --confirm-convergence
```

Replace the version and inactive VNI with your observation. `--confirm-convergence` is your acknowledgement, not an automated check. Interactive use can omit `--if-version-match`, but still requires the exact inactive VNI, the convergence flag, and terminal confirmation.

For REST, supply the observed version and inactive VNI explicitly:

```http
POST /v2/org/example-provider/nico/vpc/12345678-1234-5678-90ab-cdef01234567/routing-profile/release-inactive-vni HTTP/1.1
Host: api.example.com
Authorization: Bearer <access-token>
Content-Type: application/json

{
  "ifVersionMatch": "V2-T1789147200000000",
  "expectedInactiveVni": 12001
}
```

REST does not accept a convergence-confirmation field or perform the checks for you. A successful release returns `releasedInactiveVni` and an advanced Core version; the active VNI and profile remain unchanged. Verify that authoritative inspection now reports no retained allocation before ending the operational hold.

## Recover from an Ambiguous Response

A timeout, lost response, or invalid acknowledgement after submission can follow a committed transaction. Neither the CLI nor REST automatically retries these mutations. Inspect authoritative routing state before deciding on another action.

For a repeat of the same explicit-version CLI request or REST release, keep the original version and exact VNI selection unchanged, including omission. A stale-version error does not prove the earlier transaction failed. Do not automatically replace its precondition with a fresh version.

An interactive CLI invocation without a version and each REST profile-change request obtain a new version internally. Invoking either again approves a new action against newly observed state; it is not a replay or deduplicated retry. Inspect first, then deliberately decide whether a new action is appropriate.

The CLI applies a 300-second default timeout to each RPC attempt, configurable with `FORGE_CLIENT_REQUEST_TIMEOUT_SECS`. REST profile changes share one 50-second budget across authorization, the authoritative read, and mutation. Neither deadline bounds DPU convergence or cancels a committed Core transaction.

## Interface References

Refer to the generated interfaces for the complete options and response contracts:

- [CLI Routing State](https://github.com/dsx-ai-factory/infra-controller/blob/main/docs/manuals/nico-admin-cli/commands/vpc/vpc-routing-state.md)
- [CLI Change Routing Profile](https://github.com/dsx-ai-factory/infra-controller/blob/main/docs/manuals/nico-admin-cli/commands/vpc/vpc-change-routing-profile.md)
- [CLI Release Inactive VNI](https://github.com/dsx-ai-factory/infra-controller/blob/main/docs/manuals/nico-admin-cli/commands/vpc/vpc-release-inactive-vni.md)
- [REST VPC API Reference](/infra-controller/rest-api-reference/api-reference/vpc)
