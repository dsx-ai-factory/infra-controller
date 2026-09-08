# Security Policy: NVIDIA Infra Controller (NICo)

NVIDIA Infra Controller (NICo) is a control plane for site-local bare-metal
lifecycle management. This policy covers the repository at the root of this
clone, including the Rust Core services, Go REST services, managed-host and DPU
agents, deployment charts, and supporting build artifacts.

## Reporting a Vulnerability

If you discover a potential security vulnerability, please **do not open a public
issue, pull request, discussion, or chat thread with exploit details**.

- Report via the [NVIDIA Vulnerability Disclosure Program](https://www.nvidia.com/en-us/security/) (preferred).
- E-Mail: [psirt@nvidia.com](mailto:psirt@nvidia.com)
  - We encourage you to use the following PGP key for secure email communication:
    [NVIDIA public PGP Key](https://www.nvidia.com/en-us/security/pgp-key)
- GitHub: Use the **Security** tab > **Report a vulnerability** to submit a
  private report directly on this repository.

Please include the following information:

- Product/project name and affected version, branch, image tag, or commit.
- Affected component, such as NICo Core, NICo REST, site-agent, DPU agent, PXE,
  DHCP, DNS, hardware-health, admin CLI, Helm chart, or deployment script.
- Type of vulnerability, affected interface, and required privileges.
- Step-by-step reproduction instructions and any proof-of-concept code.
- Expected impact, including effects on host lifecycle, tenant isolation,
  credentials, inventory integrity, network isolation, or availability.
- Relevant logs, audit entries, metrics, configuration snippets, and deployment
  mode, with secrets and customer/site identifiers removed.

Detailed reports help NVIDIA evaluate and address issues faster. NVIDIA's PSIRT
team will acknowledge receipt, validate severity, develop fixes, and publish
security bulletins as appropriate.

NVIDIA does not offer a bug bounty program for this project. Externally
reported security issues may receive acknowledgement when addressed under
NVIDIA's coordinated vulnerability disclosure policy. See NVIDIA's
[Product Security portal](https://www.nvidia.com/en-us/security) and
[PSIRT policies](https://www.nvidia.com/en-us/security/psirt-policies/) for
additional information.

## Security Architecture & Context

NICo is an API-based microservice stack for bare-metal lifecycle management. It
orchestrates hardware inventory, Redfish-based hardware management, firmware and
validation flows, IP address allocation, DNS, power control, provisioning,
wiping, node release, and tenant-switch trust enforcement.

This software operates at the infrastructure control-plane level. Its primary
security responsibilities are to protect privileged hardware-management actions,
site and tenant lifecycle state, bare-metal tenant isolation, bootstrap and
machine identity material, and credentials used to manage BMCs, UEFI, DPUs,
switches, fabrics, databases, and service-to-service communication.

**Repository Exposure Classification:** Public.
Basis: the canonical repository is
`github.com/dsx-ai-factory/infra-controller`, and the checked-in documentation
links to public NICo documentation. Treat this document as public-consumable
and do not add internal hostnames, private IP allocations, customer identifiers,
incident IDs, or exploit details.

**Service Exposure Classification:** External / Regulated (medium confidence).
Basis: NICo is documented as a preview infrastructure product with public docs,
containerized deployment artifacts, Helm charts, REST and gRPC APIs, admin CLI
workflows, and site-local services that can be consumed by operators, ISVs, and
higher-level orchestration systems. Individual deployments are expected to be
site-local, but the software controls production-grade privileged infrastructure
and handles sensitive credentials and tenant state.

### Major Components and Trust Boundaries

- **NICo Core (`nico-api`)**: Rust gRPC control-plane service built around the
  `forge.Forge` API. It listens on the configured API address, uses Rustls and
  SPIFFE-aware certificate processing for TLS/mTLS deployments, authorizes Forge
  calls through internal RBAC and optional Casbin policy, and can serve an admin
  web UI under `/admin` when enabled.
- **NICo REST**: Go/Echo HTTP API that fronts provider, tenant, instance,
  credential, measured-boot, network-security-group, site-explorer, and related
  workflows. Versioned routes require Bearer JWT processing through configured
  JWKS issuers or Keycloak. REST workflows use Temporal and site-agent/Core gRPC
  proxying to reach site-local Core services.
- **Managed-host and DPU agents**: Scout runs for limited lifecycle phases on hosts for inventory,
  cleanup, validation, and health checks. The DPU agent runs persistently on the
  BlueField DPU, polls Core for desired state, configures HBN/network isolation,
  manages local DHCP and metadata service behavior, and applies updates or
  hotfixes.
- **PXE, cloud-init, DHCP, DNS, and metadata services**: PXE serves boot scripts,
  cloud-init data, static boot artifacts, Scout firmware scripts, and bootstrap
  CA material. DPU-side DHCP handles host DHCP locally. FMDS-style metadata
  endpoints expose instance metadata and machine identity to the host-facing
  side of a managed machine.
- **Hardware and fabric integrations**: NICo interacts with BMCs, DPU BMCs,
  Redfish endpoints, NVUE/RMS/UFM-style fabric controllers, power shelves,
  switches, and firmware tooling. These integrations are privileged and must be
  scoped to the intended site-management networks.
- **State and secrets**: Persistent state is stored in PostgreSQL. Credential and
  certificate material is expected to be stored in Vault and synchronized through
  Kubernetes Secret mechanisms where deployed. Temporal histories, audit logs,
  OpenTelemetry/Loki/Sentry output, and Prometheus metrics are separate data
  surfaces that must not receive plaintext secrets.
- **Deployment boundary**: The reference deployment uses Kubernetes, Helm,
  cert-manager, Vault, External Secrets Operator, PostgreSQL, Temporal, MetalLB,
  telemetry services, and optional Keycloak. Cluster RBAC, NetworkPolicy, ingress
  controls, certificate issuers, and secret distribution are part of the security
  boundary, not merely operational details.

### Entry Points and Sensitive Interfaces

- Core gRPC API and reflection routes for Forge methods.
- Optional Core admin UI under `/admin`.
- Core metrics and profiler endpoints, plus per-object metrics when enabled.
- REST `/v*/org/:orgName/<apiName>/...` API routes and public health or
  `.well-known` routes.
- REST `/auth` Keycloak routes when Keycloak is configured.
- Site-agent, flow, site-manager, Temporal worker, and manager gRPC services.
- PXE `/public`, `/api/v0/pxe`, `/api/v0/cloud-init`, and `/api/v0/tls` routes.
- DPU-host metadata routes under `/latest` and `/2009-04-04`.
- DHCP, DNS, hardware-health, OpenTelemetry, and Prometheus scrape endpoints.
- CLI and configuration-file inputs from `nico-admin-cli`, Core configuration,
  REST configuration, Helm values, bootstrap scripts, and local development
  profiles.

### Security Controls Present in the Codebase

- TLS/mTLS support for Core gRPC, REST-to-Temporal, site-agent-to-Core, and other
  service-to-service paths, with cert-manager/Vault PKI support in deployment
  artifacts.
- SPIFFE-aware client certificate processing and Core authorization middleware
  with internal RBAC and optional Casbin policy.
- REST Bearer JWT validation through issuer-specific processors, JWKS
  configuration, optional Keycloak integration, organization membership, and
  role checks.
- Request body limits, server timeouts, secure headers, optional REST rate
  limiting, structured audit logging, and selected audit-body redaction.
- AES-GCM encryption/redaction helpers for selected REST-to-Temporal secret
  payloads, so sensitive fields do not need to appear in cleartext workflow
  histories.
- Vault-based credential and certificate management paths, including Kubernetes
  auth and scoped policy templates for NICo-managed credential prefixes.
- DPU-enforced network isolation, network security group modeling, measured boot
  trust APIs, machine identity signing, and lifecycle cleanup/release flows.
- Metrics and structured security events for authorization denials, missing auth
  context, TLS connection failures, DHCP packet drops, Vault errors, and related
  operational signals.

## Threat Model

The following scenarios represent the primary security concerns for this
repository. They are derived from the checked-in services, deployment templates,
and support scripts.

1. **Unauthorized lifecycle or credential control through API misconfiguration:**
   NICo Core and NICo REST expose operations that can create or release
   instances, rotate BMC/UEFI credentials, approve measured-boot trust, modify
   networking, update firmware, power machines, and change site inventory. If
   Core is run with plaintext listener modes, `bypass_rbac=true`, an unintended
   Casbin policy, permissive authorization, or an overly broad REST JWT issuer or
   Keycloak mapping, a caller could obtain privileged control-plane effects.

2. **PXE and cloud-init artifact exposure outside the managed-host network:**
   The PXE service serves boot scripts, cloud-init data, static artifacts, Scout
   firmware scripts, and bootstrap CA material based on the booting machine
   context. If PXE routes are reachable from networks other than the intended
   provisioning paths, or if proxying changes the observed client IP semantics,
   an unauthorized host could retrieve bootstrap material or cause incorrect
   provisioning behavior.

3. **DPU metadata and machine-identity misuse from the host-facing boundary:**
   The DPU metadata service exposes instance metadata, user data, phone-home, and
   machine-identity routes to the managed host. If the service is bound to the
   wrong interface, lacks the expected host/DPU isolation, or the signing proxy
   and rate-limit settings are misconfigured, tenant workloads or compromised
   hosts could access metadata outside their intended context or overuse signing
   paths.

4. **Secret disclosure through workflow, audit, telemetry, or support surfaces:**
   NICo handles BMC passwords, UEFI credentials, registry pull secrets, JWTs,
   service-account credentials, Vault tokens, TLS private keys, cloud-init user
   data, machine identity material, and hardware-management access tokens. REST
   audit redaction and Temporal encryption cover selected fields, but new
   handlers, logs, traces, metrics, Sentry events, generated support bundles, or
   workflow payloads can leak secrets if they bypass those helpers or introduce
   new sensitive fields without redaction.

5. **Hardware-management abuse through Redfish, firmware, switch, or power paths:**
   Site Explorer, component managers, firmware update flows, Redfish clients,
   power-shelf and switch controllers, and Scout firmware scripts operate against
   hardware-management interfaces. A compromised control-plane identity,
   tampered inventory, or unauthorized firmware artifact could disrupt hardware,
   alter boot configuration, weaken trust state, or make machines unavailable.

6. **Tenant network-isolation bypass through DPU or fabric configuration drift:**
   NICo generates and applies HBN, NVUE, DHCP, VPC, route-target, network
   security group, InfiniBand, and NVLink state. Incorrect site configuration,
   unsafe stateful ACL enablement, stale DPU state, inconsistent route profiles,
   or controller bugs could leak traffic between tenants, management networks,
   or maintenance domains.

7. **Sensitive observability or admin surface exposure:** Core metrics,
   per-object metrics, profiler endpoints, admin UI pages, REST metrics,
   hardware-health metrics, Loki logs, OpenTelemetry streams, and audit tables
   can reveal machine identifiers, site topology, tenant lifecycle state,
   authorization failures, or operational timing. Browser-facing REST deployments
   must also review CORS behavior and ingress controls so authenticated sessions
   are not exposed to unintended origins.

## Critical Security Assumptions

- Production deployments use TLS/mTLS for Core, site-agent, REST-to-Temporal,
  and service-to-service traffic. Plaintext listener modes and development
  certificates are for local testing only.
- Core authorization is enforced in production. `bypass_rbac`, permissive Casbin
  mode, temporary policy files, and extra CLI certificate allowlists are reviewed
  before deployment and are not enabled by accident.
- REST deployments configure exactly the intended JWT issuers, audiences, scopes,
  claim mappings, Keycloak settings, service-account roles, and organization
  mappings. Unconfigured or duplicate issuer behavior is treated as a deployment
  failure.
- Kubernetes ingress, Service types, NetworkPolicies, firewall rules, MetalLB
  address pools, and management VLANs expose each service only to its intended
  peers. In particular, PXE, DHCP, DNS, FMDS, metrics, profiler, admin UI,
  Temporal, PostgreSQL, and Vault endpoints are not broadly reachable.
- The DPU is a trusted enforcement point. DPU OS integrity, DPU firmware, HBN,
  OVS/NVUE state, kernel isolation, and host-facing interface placement must be
  protected independently of NICo application code.
- Vault, cert-manager, External Secrets Operator, Kubernetes Secret storage, and
  PostgreSQL are deployed securely, backed up appropriately, and access-scoped to
  the NICo service accounts that require them.
- Temporal namespaces, task queues, histories, and encryption keys are protected.
  Any workflow payload carrying secrets must use the shared redaction/encryption
  helpers or an equivalent reviewed mechanism.
- Audit logs, traces, metrics, support bundles, Sentry events, and log archives
  are treated as sensitive operational data. New fields carrying passwords,
  tokens, private keys, user data, machine identities, or customer/site-specific
  values must be redacted or omitted.
- Hardware-management endpoints are reachable only from authorized management
  networks, and their factory/default credentials are rotated before production
  use.
- PXE and cloud-init artifacts are generated only from trusted configuration and
  served only to the machine they are intended to bootstrap. Static boot artifact
  directories are controlled by the deployment and are not writable by tenants.
- Machine identity signing keys, measured-boot trust approvals, and tenant
  identity signing configuration are treated as root-of-trust material.
- Operators validate site-specific Helm values, bootstrap scripts, and generated
  configuration before applying them to production clusters. Examples and local
  development defaults are not production hardening guidance.
- The software is provided "as is" without warranties of any kind. Features,
  APIs, and configurations may change in future releases. For production
  deployments, please test thoroughly in non-critical environments first.

## Supported Versions and Security Updates

The software is provided "as is" without warranties of any kind. Features,
APIs, and configurations may change in future releases. For production
deployments, please test thoroughly in non-critical environments first. Security
fixes should be consumed from maintained release branches, tagged container
images, or commits designated by the NICo maintainers and NVIDIA PSIRT.
Operators should test updates in a non-critical environment before upgrading
production control planes.

## Deployment Hardening Checklist

- Require TLS/mTLS for Core, site-agent, Temporal, and service-to-service gRPC.
- Disable development-only plaintext modes, debug logging, permissive auth,
  `bypass_rbac`, and local development certificates in production.
- Restrict PXE, DHCP, DNS, FMDS, metrics, profiler, admin UI, Temporal,
  PostgreSQL, Vault, and hardware-management interfaces to intended networks.
- Configure REST JWT issuers, Keycloak, audiences, scopes, claim mappings, CORS,
  and rate limits for the deployment's trusted clients.
- Store BMC, UEFI, switch, fabric, registry, database, and service-account
  credentials in Vault or Kubernetes Secret flows designed for the deployment.
- Validate Helm values and site configuration for real site ranges, route
  targets, network security group limits, certificate issuers, and secret
  references before deployment.
- Monitor authorization denials, auth-context-missing events, TLS failures, DHCP
  drops, Vault errors, controller reconciliation failures, and credential
  rotation status.
- Keep REST SDKs, generated API references, Helm charts, deployment manifests,
  and documentation in sync with any security-relevant behavior changes.

## Scope and Out of Scope

In scope:

- Source code, generated interfaces, tests, deployment manifests, Helm charts,
  bootstrap scripts, container build files, and documentation in this repository.
- NICo Core, NICo REST, site-agent, DPU agent, Scout, PXE, DHCP, DNS,
  hardware-health, admin CLI, workflow, and manager services.
- Vulnerabilities that affect confidentiality, integrity, or availability of
  NICo-managed hosts, tenants, credentials, networks, firmware, boot flows,
  identity material, or lifecycle state.

Out of scope for this repository's maintainers, but still relevant to secure
operation:

- Vulnerabilities in third-party services deployed beside NICo unless NICo's
  configuration or integration creates the issue.
- Site-specific network design, firewall policy, certificate authority
  operations, identity provider policy, hardware lifecycle policy, and physical
  security controls.
- Public disclosure of exploit details before NVIDIA PSIRT completes coordinated
  vulnerability handling.

## Dependency Security

The Rust workspace uses Tokio, Tonic, Axum, Rustls, SQLx, Casbin, OAuth/JWT,
Vault, Kubernetes, OpenTelemetry, Prometheus, DHCP, and cryptographic libraries.
The Go REST workspace uses Echo, JWT, go-jose, Temporal, Bun/PostgreSQL, gRPC,
Viper/Cobra, Vault APIs, OpenTelemetry, Prometheus, and Sentry. Dependency
updates must account for generated protobuf/REST interfaces, deployment images,
license checks, banned dependency checks, and the pinned toolchains documented
in the repository.
