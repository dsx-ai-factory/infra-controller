# `.nico` DNS Zone — Service Endpoint Reference

## Overview

NICo (ncx-infrastructure-controller) depends on a set of well-known hostnames in the `.nico` DNS zone. These names are resolved by DPU agents, host PXE loaders, and other in-band management components at runtime. Several of these hostnames are **compiled into binaries or embedded shell scripts** and cannot be changed without rebuilding the software.

Before deploying NICo, you must configure DNS A records that resolve each
`.nico` hostname to the appropriate service virtual IP (VIP) on your
out-of-band (OOB) management network. Configure AAAA records for services that
must be discovered over IPv6; in particular, DHCPv6 option 56 requires an AAAA
record for the NTP hostname. A site-local recursive resolver (Unbound or
equivalent) running on your site controller is the recommended approach.

Stock DPU-agent images query `carbide-ntp.forge`. The rebranded
`nico-ntp.nico` name applies only to images built to use that name. These are
distinct hostnames, not suffix substitutions; configure the exact name queried
by the deployed agent, or serve both aliases during a mixed-image transition.

---

## Endpoint Quick Reference

| Hostname | Port | Protocol | Consumers | Backing Service | Purpose |
|---|---|---|---|---|---|
| `nico-api.nico` | 443 | gRPC / TLS | DPU agents, admin CLI, PXE service, DHCP plugin, FMDS, health probe | `nico-api` pod | NICo gRPC API |
| `nico-pxe.nico` | 80 | HTTP | DPU agents, iPXE clients | `nico-pxe` pod | iPXE scripts, cloud-init payloads, boot artifacts, internal APT |
| `nico-static-pxe.nico` | 80 | HTTP | Host PXE loader (scout) | `nico-static-pxe` pod | Static boot files: `scout.squashfs`, `scout.efi`, BFB images |
| `carbide-ntp.forge` (stock) or `nico-ntp.nico` (rebranded image) | 123 | UDP (NTP) | DPU agents, managed hosts (DHCP options 42 and 56) | `nico-ntp` pods or operator NTP service | NTP time synchronisation |
| `unbound.nico` | 53 | UDP / TCP (DNS) | DPU agents, managed hosts (DHCP option 6) | `nico-unbound` pod | Site-local recursive DNS resolver |
| `otel-receiver.nico` | 443 | gRPC / TLS (OTLP) | DPU otel-collector sidecars | otel-receiver service | OpenTelemetry ingestion endpoint |
| `socks.nico` | 1888 | SOCKS5 | DPU agent extension service pods | SOCKS5 proxy service | Outbound HTTP/HTTPS proxy for DPU-hosted workloads |

---

## Endpoint Details

### `nico-api.nico` — NICo gRPC API

**Port:** 443 (TLS)  
**Protocol:** gRPC over TLS  
**In-cluster address:** `nico-api.nico-system.svc.cluster.local:1079`

The primary NICo management API. All management-plane components communicate with NICo through this address. Clients on the OOB network connect on port 443; the pod itself listens on port 1079.

**Consumers:**
- `nico-agent` (DPU agent) — phones home for network configuration, workflow state, and provisioning instructions
- `nico-admin-cli` — operator administration over gRPC
- `nico-pxe` — fetches machine records to generate per-machine cloud-init and iPXE scripts
- `nico-dhcp` DHCP plugin — queries the API during DHCPDISCOVER
- NICo Metadata Service (`fmds`) — metadata queries and distribution
- `health` probe service — periodic health polling

**Configurability:** Most services accept a `NICO_API_URL` environment variable or an equivalent config file entry to override this address. The compiled default in binaries and config files is `https://nico-api.nico`. Because this is a default rather than a hardcoded constant, it can be overridden at deploy time without rebuilding.

---

### `nico-pxe.nico` — PXE / Boot Service

**Port:** 80 (HTTP)  
**Protocol:** HTTP  
**In-cluster address:** `nico-pxe.nico-system.svc.cluster.local`

Serves dynamic per-machine iPXE boot scripts, cloud-init payloads, boot artifacts, and the internal APT package repository to DPU agents and PXE-booting clients.

**Consumers:**
- `nico-agent` (DPU agent) — resolves this hostname at startup to locate boot artifacts and the internal APT repository (paths under `/public/blobs/internal/`)
- iPXE clients during initial machine network boot

**Configurability:** The DPU agent (`crates/agent/src/main_loop.rs`) resolves `nico-pxe.nico` directly via DNS. **This lookup is not overridable via config in the compiled agent.** The PXE service itself accepts a `NICO_PXE_URL` environment variable to override the URL it advertises to clients, but the agent's DNS lookup for this name is fixed.

> **Warning:** `nico-pxe.nico` is hardcoded in the compiled `nico-agent` binary (`crates/agent/src/main_loop.rs`). This DNS record must exist and resolve correctly on the OOB network for DPU agents to function. Changing this hostname requires rebuilding the DPU agent.

---

### `nico-static-pxe.nico` — Static Boot Asset Server

**Port:** 80 (HTTP)  
**Protocol:** HTTP  
**In-cluster address:** `nico-static-pxe.nico-system.svc.cluster.local`

Serves pre-built, version-controlled boot assets used during host bring-up. Unlike `nico-pxe.nico`, content here is static rather than dynamically generated per machine.

**Consumers:**
- Scout host PXE loader — downloads `scout.squashfs` (the initramfs), `scout.efi`, and BFB images used during host network boot and DPU firmware provisioning

**Configurability:** The URL is hardcoded in host boot shell scripts (`pxe/common_files/scout-loader-rclocal`, `pxe/common_files/check-scout-updates.sh`) that are embedded in boot images at build time. The server-side deployment can set `NICO_STATIC_PXE_URL` to override the URL used by the PXE service, but the embedded client scripts that run on hosts **cannot be reconfigured at runtime**.

> **Warning:** `nico-static-pxe.nico` is hardcoded in host boot scripts compiled into boot images (`pxe/common_files/scout-loader-rclocal`, `pxe/common_files/check-scout-updates.sh`). Changing this hostname requires rebuilding all host boot images.

---

### `carbide-ntp.forge` / `nico-ntp.nico` — NTP Service

**Port:** 123 (UDP)  
**Protocol:** NTP  
**In-cluster addresses:** `nico-ntp-1.nico-ntp.nico-system.svc.cluster.local`, `nico-ntp-2.nico-ntp.nico-system.svc.cluster.local`, `nico-ntp-3.nico-ntp.nico-system.svc.cluster.local`

Provides NTP time synchronisation for DPU agents and managed hosts. The bundled
service is backed by three pods for redundancy. Configure multiple DNS A
records pointing to its IPv4 VIPs. To advertise the service through DHCPv6
option 56, also configure one or more AAAA records pointing to reachable IPv6
NTP endpoints.

**Consumers:**
- `nico-agent` (DPU agent) — stock images resolve `carbide-ntp.forge` at startup to discover NTP server addresses; images rebuilt with the rebranded name resolve `nico-ntp.nico`. IPv4 and IPv6 results are advertised to managed hosts through DHCP options 42 and 56, respectively

**Configurability:** The stock DPU agent resolves `carbide-ntp.forge` directly
via DNS (`crates/agent/src/util.rs`). **This lookup is not overridable via
config in the compiled agent.** Using `nico-ntp.nico` requires an image rebuilt
with that hostname.

> **Warning:** `carbide-ntp.forge` is hardcoded in the stock `nico-agent`
> binary. That exact DNS record must resolve correctly on the OOB network.
> Changing the hostname requires rebuilding the DPU agent.

**Multiple records (recommended):** Configure one A record per IPv4 NTP pod
instance to provide redundancy. Configure one or more AAAA records when using
the DHCPv6 option-56 fallback. Clients receive every address from the matching
family and select among them.

---

### `unbound.nico` — Recursive DNS Resolver

**Port:** 53 (UDP and TCP)  
**Protocol:** DNS  
**In-cluster service:** `nico-unbound` in the `nico-system` namespace

The site-local recursive DNS resolver. DPU agents and managed hosts use this resolver for all DNS lookups, including resolution of other `.nico` names. The resolver address is distributed to clients via DHCP option 6.

**Consumers:**
- DPU agents — all DNS resolution during provisioning and normal operation
- Managed host operating systems — configured as the primary name server via DHCP

**Configurability:** The resolver address is not compiled into binaries; it is distributed to clients via DHCP and can be changed by updating the DHCP server configuration. The `.nico` zone data must be loaded into Unbound (see [DNS Configuration](#dns-configuration) below) for all other hostnames in this document to resolve correctly.

---

### `otel-receiver.nico` — OpenTelemetry Receiver

**Port:** 443 (TLS)  
**Protocol:** gRPC / TLS (OTLP — OpenTelemetry Protocol)

Ingests telemetry (metrics, traces, and logs) exported by otel-collector sidecars running on managed BlueField DPUs.

**Consumers:**
- `nico-otelcol` — the otel-collector sidecar deployed on each DPU via the `bluefield/charts/nico-otelcol` Helm chart
- Site controller otel agents (`bluefield/otel/site-controller/`)

**Configurability:** The endpoint is set in otel-collector configuration YAML files (`bluefield/charts/nico-otelcol/files/otel_config.yaml`, `bluefield/otel/otel_config.yaml`, `bluefield/otel/site-controller/otel_config.yaml`). Changing the address requires updating those files and redeploying the otel-collector.

---

### `socks.nico` — SOCKS5 Outbound Proxy

**Port:** 1888  
**Protocol:** SOCKS5

Provides outbound HTTP/HTTPS connectivity for Kubernetes workloads launched by the DPU agent as extension services. The agent sets `HTTP_PROXY=socks5://socks.nico:1888` and `HTTPS_PROXY=socks5://socks.nico:1888` in the environment of every extension service pod it starts.

**Consumers:**
- Kubernetes pods launched by the DPU agent as extension services (`crates/agent/src/extension_services/k8s_pod_handler.rs`)

**Configurability:** The proxy address and port are **hardcoded in the compiled `nico-agent` binary** (`crates/agent/src/extension_services/k8s_pod_handler.rs`). Changing this address requires rebuilding the DPU agent.

> **Warning:** `socks.nico:1888` is hardcoded in the compiled `nico-agent` binary (`crates/agent/src/extension_services/k8s_pod_handler.rs`).

---

## Network Topology

`.nico` service endpoints are hosted on the site controller (control plane). All service VIPs have their routes injected into both the underlay and the overlay.

**DPU agents** and **managed hosts** reach `.nico` endpoints over the OOB/admin management network. All `.nico` names must be resolvable from this network path.

**Tenant workloads** can reach the service VIPs at the IP level but are not configured to use `unbound.nico` as their DNS resolver and will not resolve `.nico` names.

---

## Resolution Flow for DPU Agents and Unallocated Managed Hosts

This section describes how DNS queries flow from the two client types that consume the `.nico` zone: DPU agents (during DPU provisioning and steady-state operation) and managed hosts that have been ingested but not yet allocated to a tenant.

### DPU Agents

The DPU agent (`nico-agent`) issues DNS queries at startup and periodically thereafter — for fetching FMDS configuration, pulling boot artifacts from the PXE service, reaching NTP, exporting telemetry to the OTel receiver, and dialing the SOCKS proxy used by extension-service pods.

How it finds its resolver:

- The DPU's network interface receives an IP and DHCP options from `nico-dhcp` (the Kea + nico hook combination).
- DHCP option 6 (Domain Name Server) is set to the `unbound.nico` VIP. This value comes from the Kea hook parameter `nico-nameserver`.
- The DPU agent uses this as its sole resolver. The agent has no compiled-in resolver address; changing the resolver is a DHCP-side configuration change.

What it resolves:

| Query | How it's answered |
|---|---|
| `nico-api.nico`, `nico-pxe.nico`, `nico-static-pxe.nico`, the deployed agent's exact NTP hostname, `socks.nico`, `otel-receiver.nico` | Served locally by Unbound from `local_data.conf` |
| Names in the site domain (e.g., a `<machine-id>` record under `initial_domain_name`) | Reaches `nico-dns` via upstream delegation of the site zone to the `nico-dns` VIPs, or via an explicit forward zone in Unbound |
| External names (package mirrors, public NTP fallbacks, etc.) | Unbound forwards or recurses to the upstream resolver configured in `forwarders.conf` |

The DPU agent does not query the Kubernetes cluster DNS (CoreDNS); it has no awareness of the `*.svc.cluster.local` namespace.

### Unallocated Managed Hosts

A managed host that has been ingested but not yet allocated to a tenant remains on the admin network. Its DNS configuration mirrors a DPU agent's:

- The host receives DHCP from `nico-dhcp` on its admin-network interface.
- DHCP option 6 hands out the `unbound.nico` VIP.
- The host OS's resolver (`/etc/resolv.conf`, populated by NetworkManager, systemd-networkd, or cloud-init depending on the image) uses Unbound for all queries.

What it resolves:

- The same service endpoints as DPU agents — for example, `nico-pxe.nico` for cloud-init userdata and the internal APT repository, the NTP hostname compiled into the deployed agent for time synchronisation, and `nico-api.nico` for any in-band tooling that targets the NICo API.
- The host's own hostname and other site-zone records, through the same delegation or forward-zone path that DPU agents use.
- External names, through Unbound's upstream forwarder.

Once a tenant is assigned to the host, the host typically receives tenant-provided cloud-init userdata that may reconfigure DNS. Tenant-allocated DNS behaviour is outside the scope of this page.

### Common Properties

For both client types:

- All resolution flows through Unbound. Neither DPU agents nor unallocated hosts contact `nico-dns` directly — they reach it (when they need to) only via the upstream delegation chain or an Unbound forward zone.
- Reachability requires the `unbound.nico` VIP to be routable on the OOB/admin management network.
- Site-zone names resolve only if the `initial_domain_name` zone is delegated to the `nico-dns` VIPs at the upstream authoritative DNS, or if Unbound is configured with an explicit `forward-zone:` (or `stub-zone:`) pointing at those VIPs. If neither is in place, site-zone queries fail silently from the client's perspective — DPU agents will still resolve the hardcoded `.nico` names but cannot look up per-machine records.

---

## Hardcoded vs. Configurable Endpoints

| Hostname | Hardcoded in | Configurable at deploy time? |
|---|---|---|
| `nico-api.nico` | Default value only (`crates/host-support/src/agent_config.rs`, config defaults across services) | **Yes** — override via `NICO_API_URL` env var or config file |
| `nico-pxe.nico` | Compiled into `nico-agent` (`crates/agent/src/main_loop.rs`) | **No** — requires rebuilding `nico-agent` |
| `nico-static-pxe.nico` | Embedded in host boot scripts (`pxe/common_files/scout-loader-rclocal`, `pxe/common_files/check-scout-updates.sh`) | **No** — requires rebuilding host boot images |
| `carbide-ntp.forge` (stock) / `nico-ntp.nico` (rebranded image) | Compiled into `nico-agent` (`crates/agent/src/util.rs`) | **No** — requires rebuilding `nico-agent` |
| `unbound.nico` | Not compiled into binaries; distributed via DHCP option 6 | **Yes** — update DHCP server configuration |
| `otel-receiver.nico` | otel-collector config YAML (`bluefield/charts/nico-otelcol/files/otel_config.yaml`, etc.) | **Yes** — update otel-collector config files and redeploy |
| `socks.nico` | Compiled into `nico-agent` (`crates/agent/src/extension_services/k8s_pod_handler.rs`) | **No** — requires rebuilding `nico-agent` |

---

## DNS Configuration

### Using Unbound (`local_data.conf`)

For Helm deployments, populate `unbound.localData` with each service VIP. The
chart renders IPv4 literals as A records and IPv6 literals as AAAA records. For
the legacy Kustomize deployment, populate
`deploy/files/unbound/local_data.conf`; its checked-in placeholders cover the
IPv4 service VIPs and both NTP hostname aliases, so add a site-specific AAAA
entry for `carbide-ntp.forge` when stock agents use the DHCPv6 option-56
fallback. Add the corresponding `nico-ntp.nico` record only for rebranded
images. The Unbound pod restarts automatically once the generated ConfigMap
changes.

### Using Other DNS Providers

Any DNS server that can serve authoritative responses for the required `.nico`
and `.forge` names on your OOB management network is supported. Create A
records for each applicable hostname and an AAAA record for the exact NTP
hostname queried by the deployed agent when DHCPv6 option 56 is required.

> **Note:** `.nico` and `.forge` are private service zones in this deployment.
> They should not be forwarded to public resolvers. Configure your DNS server
> to answer the required names locally with no upstream forwarding.

---

## Deployment Checklist

After configuring DNS, verify that all records resolve correctly from a host or DPU on the OOB management network:

```bash
for name in nico-api.nico nico-pxe.nico nico-static-pxe.nico \
            unbound.nico otel-receiver.nico socks.nico; do
    printf "%-30s -> %s\n" "$name" "$(dig +short "$name" @<UNBOUND_VIP> || echo 'FAILED')"
done

# Stock agent: both results are required for dual-stack NTP fallback.
dig +short A carbide-ntp.forge @<UNBOUND_VIP>
dig +short AAAA carbide-ntp.forge @<UNBOUND_VIP>
```

For an image rebuilt to query `nico-ntp.nico`, replace the complete
`carbide-ntp.forge` hostname in both commands. Do not replace only its TLD.

Verify reachability on expected ports:

```bash
# NICo gRPC API (TLS handshake)
openssl s_client -connect nico-api.nico:443 </dev/null 2>/dev/null | grep -E "^(subject|Verify)"

# PXE service
curl -sf --max-time 5 http://nico-pxe.nico/ -o /dev/null && echo "nico-pxe OK" || echo "nico-pxe FAILED"

# Static PXE service
curl -sf --max-time 5 http://nico-static-pxe.nico/ -o /dev/null && echo "nico-static-pxe OK" || echo "nico-static-pxe FAILED"

# NTP
ntpdate -q carbide-ntp.forge

# DNS resolver (should return a result for an external name)
dig +short +timeout=3 example.com @unbound.nico

# OTEL receiver (TLS handshake)
openssl s_client -connect otel-receiver.nico:443 </dev/null 2>/dev/null | grep -E "^(subject|Verify)"

# SOCKS proxy (TCP connect)
nc -zv socks.nico 1888
```

See `helm/PREREQUISITES.md` for additional deployment prerequisites.
