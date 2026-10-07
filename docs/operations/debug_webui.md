---
title: "NICo Debug WebUI"
description: "Overview of the NICo administrative web interface, authentication modes, and available views."
---

# NICo Debug WebUI

NICo includes a built-in administrative web interface intended for operational debugging and inspection. It is served at the `/admin` path of the NICo API server and provides read-oriented views of infrastructure state alongside a limited set of administrative actions.

<Warning>
The WebUI defaults to Basic authentication. Helm installations generate and
persist a password in `nico-api-web-basic-auth`. As a last-resort safeguard for
non-Helm or older deployment manifests, if `CARBIDE_WEB_BASIC_AUTH_PASSWORD` is
unset or empty, NICo generates a temporary 32-character password for that
process and logs it at warning level. That fallback password is not persisted
and changes on every process launch. Use the WebUI only through the
TLS-protected endpoint.
</Warning>

## Authentication

For Helm installations, configure `nico-api.webAuth.mode`. The default is
`basic`; `oauth2` and `none` are explicit alternatives. A
`CARBIDE_WEB_AUTH_TYPE` entry in `nico-api.extraEnv` takes precedence as a
backward-compatibility contract.

| Value | Behavior |
|-------|----------|
| *(unset)* or `basic` | HTTP Basic Auth with fixed username `admin`. Uses `CARBIDE_WEB_BASIC_AUTH_PASSWORD`, or a temporary password reported in the service logs when unset or empty. |
| `oauth2` | Microsoft Entra (Azure AD) OIDC via PKCE flow. Group-based access enforcement via MS Graph API. |
| `none` | No in-process authentication. A warning is logged at startup; restrict access using network controls or an authenticating proxy. |

For a default Helm installation, retrieve the generated password with the
`kubectl` command printed in the release notes. To use an operator-managed
credential instead:

```yaml
nico-api:
  webAuth:
    mode: basic
    basic:
      existingSecret:
        name: nico-web-password
        key: password
```

### OAuth2 (Entra) Configuration

When Helm's `nico-api.webAuth.mode` is `oauth2` (or the legacy
`CARBIDE_WEB_AUTH_TYPE=oauth2` override is used), provide the following
provider settings through `nico-api.extraEnv`:

| Variable | Description |
|----------|-------------|
| `CARBIDE_WEB_OAUTH2_CLIENT_ID` | Application (client) ID registered in Azure/Entra |
| `CARBIDE_WEB_OAUTH2_CLIENT_SECRET` | Client secret for communicating with MS Entra/Graph |
| `CARBIDE_WEB_OAUTH2_AUTH_ENDPOINT` | Entra authorization endpoint URL |
| `CARBIDE_WEB_OAUTH2_TOKEN_ENDPOINT` | Entra token endpoint URL (tenant-specific) |
| `CARBIDE_WEB_HOSTNAME` | Public hostname used for OAuth2 redirect URIs; must match the value registered in the Entra portal |
| `CARBIDE_WEB_PRIVATE_COOKIEJAR_KEY` | Secret key used to encrypt session cookies |
| `CARBIDE_WEB_ALLOWED_ACCESS_GROUPS` | Comma-separated list of Entra group names permitted to access the WebUI |
| `CARBIDE_WEB_ALLOWED_ACCESS_GROUPS_ID_LIST` | Comma-separated list of the corresponding Entra group UUIDs |

Sessions issued via the standard PKCE flow are persistent for the browser session. A client credentials flow is also supported for automated access (for example, CI pipelines); sessions granted via this flow expire after 10 minutes.

For step-by-step Entra registration and secret management instructions, see [Azure OIDC for Infra Controller Web UI](../playbooks/nico_web_oauth2.md).

## Available Views

The WebUI exposes views grouped by entity type. All views are read-only unless noted.

| View | Path | Description |
|------|------|-------------|
| Home | `/admin/` | NICo version, DPU agent upgrade policy, active log filter, dynamic feature flags, and operator-configured tool links |
| Machines | `/admin/machine` | All managed hosts; per-machine detail, health, validation status; assign/remove SKU on hosts (write) |
| DPUs | `/admin/dpu` | DPU inventory and per-DPU detail; DPU agent version list at `/admin/dpu/versions` |
| DPAs | `/admin/dpa` | DPA (Data Processing Accelerator) inventory |
| Hosts | `/admin/host` | Host-only view of managed machines |
| Instances | `/admin/instance` | Active instances with per-instance detail |
| Compute Allocations | `/admin/compute-allocation` | Allocation records; create new allocations (write) |
| Instance Types | `/admin/instance-type` | Defined instance types |
| Interfaces | `/admin/interface` | Network interface inventory and per-interface detail |
| VPCs | `/admin/vpc` | Virtual Private Cloud records |
| IB Partitions | `/admin/ib-partition` | InfiniBand partition configuration |
| IB Fabric | `/admin/ib-fabric` | InfiniBand fabric topology |
| NVLink | `/admin/nvlink` | NVLink domain and partition views |
| IPAM | `/admin/ipam/dhcp`, `/admin/ipam/dns`, `/admin/ipam/underlay`, `/admin/ipam/overlay` | IP address management state |
| Racks | `/admin/rack` | Rack inventory with health |
| Switches | `/admin/switch` | Switch inventory with health |
| Power Shelves | `/admin/power-shelf` | Power shelf inventory |
| Tenants | `/admin/tenant` | Tenant records and keysets |
| Machine Validation | `/admin/machine-validation` | Validation job results |
| Redfish Browser | `/admin/redfish-browser` | Browse raw Redfish endpoints; execute Redfish actions (write) |
| Explored Endpoints | `/admin/explored-endpoint` | BMC/out-of-band endpoints; power control and machine setup actions (write) |
| Logs | `/admin/logs` | Streamed log viewer |
| OS / iPXE Templates | `/admin/os`, `/admin/ipxe-template` | Operating system and boot template records |
| SKU | `/admin/sku` | SKU definitions |
| Domain | `/admin/domain` | Domain configuration |
| Resource Pools | `/admin/resource-pool` | Resource pool definitions |
| Search | `/admin/search` | Cross-entity search |

External tool links (for example, links to Grafana dashboards or UFM) configured in the NICo operator configuration are surfaced in the "Tools" sidebar and do not have fixed paths.
