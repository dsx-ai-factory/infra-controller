# Deploy DHCPv6

Use this guide to enable NICo's opt-in Dynamic Host Configuration Protocol for IPv6 (DHCPv6) service. Prepare the managed network, enable the DHCPv6 workload, and verify address allocation through the site's relay. If you also need to boot hosts over IPv6, complete [Embedded iPXE Boot Over IPv6](#embedded-ipxe-boot-over-ipv6).

The Core chart runs Kea DHCPv4 in `nico-dhcp` and, when enabled, Kea DHCPv6 in `nico-dhcp6`. The Deployments have separate pods, selectors, configuration, Services, probes, and rollouts. Both use the same image, hook library, ServiceAccount, client certificate, and application programming interface (API) identity. Both hooks call the same API through gRPC with mutual Transport Layer Security (mTLS). API role-based access control (RBAC) continues to apply.

## Network and Identity Requirements

Before enabling the DHCPv6 workload, review the following requirements:

- The DHCPv6 pod needs a routable IPv6 address on its listening interface and reachability to the API. An IPv6 Service alone does not assign an IPv6 address to a pod. Configure the cluster's Container Network Interface (CNI) plugin and eligible nodes accordingly.
- Supply a site-unique, stable server identifier consisting of exactly 12 hexadecimal digits. Keep it unchanged across pod replacements and upgrades. Kea advertises a DHCP unique identifier (DUID) based on a link-layer address (DUID-LL), with Ethernet hardware type and this identifier. It does not generate or persist a pod-local identity.
- Point IPv4 relays to the IPv4-only DHCP Service on UDP 67. Point IPv6 relays to the separate IPv6-only DHCP Service on UDP 547. A MetalLB deployment needs a distinct IPv6 virtual IP address (VIP) from an IPv6 address pool and a working route from the relay to that VIP.
- Configure relays to supply option 79, defined in [RFC 6939: Client Link-Layer Address Option in DHCPv6](https://www.rfc-editor.org/rfc/rfc6939), for clients whose DUID has no Ethernet media access control (MAC) address. These include enterprise-number DUIDs (DUID-EN) and universally unique identifier DUIDs (DUID-UUID).
- [Prepare the managed network](#prepare-the-managed-network) with an IPv6 prefix and the relay link-address before enabling DHCPv6.
- DHCPv6 runs one replica with a `Recreate` rollout. The chart's `replicas` value controls DHCPv4 only and defaults to one. Enabling DHCPv6 does not restrict that setting. Leases use pod-local memfiles. Durable lease storage and active-writer coordination remain outside this deployment's supported scope.

NICo accepts option 79 only from relay metadata, with Ethernet hardware type 1 and exactly six MAC bytes. That MAC takes precedence over a DUID-LL or DUID-LLT (link-layer address plus time) MAC. Mismatches produce a warning. If the relay supplies no usable option 79, NICo falls back to the Ethernet MAC in DUID-LL or DUID-LLT. Requests without either MAC source are dropped before API discovery (`no_mac_no_option79`). A missing or malformed client DUID is rejected even when option 79 is usable.

The relay path supports one Relay-Forward envelope with hop count zero. Nested relay envelopes or a nonzero hop count are rejected before API discovery (`nested_relay`).

Startup selects the first global-scope IPv6 address on the configured pod interface and binds Kea to that address explicitly. Use an interface with one routable pod IPv6 address. A missing address stops startup. Startup and readiness probes also require a bound IPv6 UDP 547 socket. Readiness checks the hook endpoint, and liveness checks the metrics HTTP server. These probes do not prove off-host routing or API allocation. Validate those with a representative client through the actual relay.

The IPv6 LoadBalancer Service uses `externalTrafficPolicy: Local` to preserve relay source addresses and ports. Route or advertise its VIP only through nodes with a ready DHCPv6 pod. With one replica, only its node can serve external requests. Any upstream load balancer must also preserve the relay's source address and UDP port. Source network address translation (NAT) that changes UDP 547 can prevent Kea's reply from returning to the relay.

## Prepare the Managed Network

Network definitions apply only when Core first creates a segment. Changing `prefix_v6`, `dhcpv6_link_address`, or `infer_slaac_eui64_addresses` in the site configuration does not update an existing segment. A change from an existing seed snapshot reports configuration drift. For an older segment without a snapshot, Core records the declaration without changing the segment.

The network-segment command-line interface (CLI) and remote procedure calls (RPCs) provide no in-place prefix or inference-flag update. An existing segment needs a reviewed migration to add IPv6 or enable inference. Keep the DHCPv6 workload disabled until the stored segments it will serve are ready.

For a new segment, add the following definition to the existing TOML in `nico-api.siteConfig.nicoApiSiteConfig`. Retain the rest of the site's TOML. A Helm string override replaces the entire value. Replace the segment name and addresses with unused site allocations before the segment is first created:

```toml
[networks.underlay_dual_stack]
type = "underlay"
prefix = "192.0.2.0/24"
gateway = "192.0.2.1"
prefix_v6 = "2001:db8:20::/64"
dhcpv6_link_address = "2001:db8:20::1"
mtu = 1500
reserve_first = 5
```

`prefix_v6` is an optional IPv6 prefix in Classless Inter-Domain Routing (CIDR) notation that adds a second prefix to the same segment as the IPv4 `prefix`. Omitting it leaves this definition IPv4-only. `dhcpv6_link_address` is an optional IPv6 address used for exact matching of the relay's link-address. It can be outside the IPv6 prefix and requires an IPv6 prefix to be configured. The server prefers an exact link-address match and otherwise falls back to prefix containment. `gateway` applies only to IPv4, and `reserve_first` reserves the same number of leading addresses in each prefix. [IP and Network Configuration](ip-and-network-configuration.md) describes the physical interface and segment-type rules.

`infer_slaac_eui64_addresses` is an optional per-network Boolean that defaults to `false` and applies to the whole segment. Set it to `true` only for clients that derive Stateless Address Autoconfiguration (SLAAC) addresses from their MAC addresses using modified EUI-64.

For site-relayed INFORMATION-REQUEST messages and SOLICIT messages without IA_NA, Core can infer and persist the client's SLAAC address and refresh its DNS naming. Creating an inferred address requires all of the following:

- The segment uses `allocation_strategy = "dynamic"`, which is the default. Inference is disabled for `allocation_strategy = "reserved"`.
- The segment has exactly one IPv6 prefix, and that prefix is a `/64`.
- The interface has no existing IPv6 assignment.

For a new segment, apply the site configuration through the normal API deployment workflow. Then, from your admin CLI environment, use [network-segment show](https://github.com/dsx-ai-factory/infra-controller/blob/main/docs/manuals/nico-admin-cli/commands/network-segment/network-segment-show.md#examples) with the segment ID. Inspect `PREFIXES` and `INFER SLAAC EUI-64`; the list-all view does not display the inference flag. SLAAC inference requires exactly one IPv6 `/64` and an inference value of `true`. Verify these stored values before enabling the DHCPv6 workload.

## Enable the DHCPv6 Workload

After preparing the managed network and relay path, add the following to the site's Helm values. Replace the example addresses and server identifier with site allocations. If you supply Domain Name System (DNS) servers, use reachable IPv6 recursive resolvers:

```yaml
nico-dhcp:
  dhcp:
    v6Enabled: true
    # Replace with this site's stable identifier.
    v6ServerIdentifier: "020000000001"
    # Replace with a reachable IPv6 recursive resolver.
    v6DnsServers: "2001:db8:10::53"
  v6ExternalService:
    enabled: true
    annotations:
      # Replace with the IPv6 relay VIP.
      metallb.universe.tf/loadBalancerIPs: "2001:db8:10::67"
```

Apply through the site's normal Helm upgrade workflow, using the Core image and chart from the same repository revision as required by the [image compatibility policy](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm/README.md#image-configuration). When upgrading from a revision without the `dhcp` and `v6ExternalService` maps, update both the chart and Core image. DSX, hardware-health, and PXE use their dual-stack listener defaults unless an explicit listener configuration overrides them. Setting `dhcp.v6Enabled: false` removes the v6 resources while preserving the v4 pod template. The v6 Deployment uses `Recreate` so old and replacement v6 pods do not serve simultaneously.

The example enables address allocation, not HTTP boot. Refer to [Helm Values](#helm-values) for the complete settings and defaults, then complete [Deployment Verification and Metrics](#deployment-verification-and-metrics).

## Deployment Verification and Metrics

For stateful DHCPv6, validate address allocation with a representative client through the actual relay and confirm that it receives an address from the managed IPv6 prefix. For SLAAC boot, confirm that an INFORMATION-REQUEST receives configuration without a lease and that Core records the client's inferred address. Before serving DUID-EN or DUID-UUID clients, verify that the relay supplies a usable option 79 with a representative client of that type.

Both independent DHCP metrics targets use the configured port (1089 by default). With ServiceMonitor enabled, the common `app.kubernetes.io/metrics` label discovers both through the named `http` port. Check both pod targets after the exchange:

```bash
curl --noproxy '*' -fsS 'http://192.0.2.10:1089/metrics'
curl --noproxy '*' -fsS 'http://[2001:db8:10::10]:1089/metrics'
```

Replace the example addresses with the actual v4 and v6 pod addresses. Confirm that the v6 request count increased and that the v4 endpoint remains independently reachable. [NICo Metrics](../observability/metrics.md) describes the metrics Service address-family policy and shared TCP listener behavior.

## Embedded iPXE Boot Over IPv6

Optional: complete this procedure to boot a host when DHCPv4 is unavailable. The host's UEFI firmware downloads and starts an iPXE EFI executable. That executable contains NICo's boot script, which configures networking and contacts the NICo PXE API for the next boot script.

### Check the Boot Prerequisites

Use a source revision containing the required DHCPv6 and embedded iPXE changes listed in [Implementation References](#implementation-references). Build the Core image, chart, and EFI artifacts from that revision.

For SLAAC boot, inspect `pxe/ipxe/local/embed.ipxe` at the served artifact's recorded source revision. The script must prefer `netX.dhcpv6/ip6` and fall back to `netX.ndp/ip6` when no lease exists. `netX` represents the selected interface. If you cannot verify the artifact's source revision, follow [Build and Deploy the EFI Artifacts](#build-and-deploy-the-efi-artifacts) with a revision containing the linked SLAAC implementation. A Core version or iPXE version banner alone does not prove that the served EFI artifact supports SLAAC boot.

Prepare the boot network and firmware with the following:

- A stateful DHCPv6 lease from NICo with router advertisements that set the managed-address flag (`M=1`), or the site-relayed SLAAC configuration described below. Additional SLAAC addresses are allowed with a stateful lease.
- A relay that supplies the client's Ethernet MAC address in RFC 6939 option 79. Embedded iPXE uses DUID-UUID, so it needs this relay metadata even if firmware previously used DUID-LL or DUID-LLT.
- UEFI firmware configured to HTTP boot over IPv6 and pass the boot URL to iPXE in its EFI device path. Loading an EFI file from local storage without that URL does not provide the PXE API endpoint.
- IPv6 routing to the boot server and every HTTP or HTTPS payload endpoint used by the returned boot script, including redirect targets. Provide AAAA records for hostname endpoints and advertise reachable IPv6 recursive resolvers through `dhcp.v6DnsServers`. Refer to [Helm Values](#helm-values) and [Unbound IPv6 Transport](../configuration/dns.md#unbound-ipv6-transport).

For SLAAC boot through a site DHCPv6 relay, configure all of the following:

- Set `M=0` and the other-configuration flag (`O=1`) in router advertisements.
- Advertise exactly one autonomous `/64` (`A=1`) that matches the managed segment's IPv6 prefix.
- Enable modified EUI-64 inference as described in [Prepare the Managed Network](#prepare-the-managed-network). Core uses the options-only INFORMATION-REQUEST to record iPXE's address.

This SLAAC boot path does not cover privacy addresses, multiple autonomous prefixes, or DPU-local boot. [Interface Selection](#interface-selection) explains the multi-interface limitation.

Before relying on inference, inspect the interface's stored addresses from your admin CLI environment. Use [machine-interfaces show](https://github.com/dsx-ai-factory/infra-controller/blob/main/docs/manuals/nico-admin-cli/commands/machine-interfaces/machine-interfaces-show.md#examples) to find the interface UUID by MAC address. Then use [machine-interfaces show-addresses](https://github.com/dsx-ai-factory/infra-controller/blob/main/docs/manuals/nico-admin-cli/commands/machine-interfaces/machine-interfaces-show-addresses.md#examples) with that UUID. Its `Address`, `Family`, and `Type` columns identify each stored assignment.

A DHCPv6 lease recorded during firmware bootstrap, a static address, or a fixed reservation prevents Core from adding an inferred SLAAC address. Core can reuse an already registered matching SLAAC address. A later stateful DHCPv6 allocation replaces the inferred SLAAC address, so iPXE must prefer that lease when both local addresses exist. Qualify the firmware exchange separately. Receiving an options-only reply does not prove that Core registered the address iPXE uses.

HTTPS endpoints require a certificate chain trusted by the deployed iPXE artifact. The build tasks retain upstream iPXE trust defaults and do not add a site-specific CA. A successful firmware download does not establish that iPXE trusts the same endpoint.

### Build and Deploy the EFI Artifacts

Follow [Generating Bootable Artifacts](../bootable_artifacts.md#3-build-ipxe-image) to build iPXE for each required architecture, `x86_64` or `aarch64`. The build tasks embed NICo's boot script and apply the HTTP IPv6 source-address patch.

Follow [Publish the EFI Files](../bootable_artifacts.md#publish-the-efi-files) to replace the served files while preserving the other boot artifacts. Updating DHCP configuration or Core does not replace an EFI file served separately.

### Configure the Boot Server

Set `nico-dhcp.dhcp.v6ProvisioningServer` to the provisioning server's IPv6 address in umbrella chart values. Use the bare address, without brackets, a port, a CIDR suffix, or surrounding whitespace. An empty or whitespace-only value disables generated URLs. Other values that are not IPv6 literals fail hook initialization.

For eligible UEFI HTTP boot clients, the DHCPv6 hook supplies option 59 with a URL in this format. The architecture is `x86_64` or `aarch64`:

```text
http://[<IPv6-address>]:8080/public/blobs/internal/<architecture>/ipxe.efi
```

The server must expose this path over IPv6 HTTP on TCP port 8080 to boot clients, including `ipxe.efi` for each required architecture. It must also expose `/api/v0/pxe/` on the same scheme and port. A static-file server alone is insufficient. Preserve the client's source address on PXE API requests: NICo identifies the booting interface from the connection's source IP, not `X-Forwarded-For`.

For the bundled `nico-pxe` external Services, configure the following settings under `nico-pxe.externalService` in umbrella chart values. The address-family settings apply to both external ports, 8080 and 80:

- Set `enabled` to `true`. The default is `false`.
- Use `ipFamilyPolicy: RequireDualStack` when DHCPv4 or other IPv4 PXE clients still need these Services. Set `ipFamilies: [IPv4, IPv6]` for an IPv4-primary Service or `[IPv6, IPv4]` for an IPv6-primary Service.
- Use `ipFamilyPolicy: SingleStack` with `ipFamilies: [IPv6]` only when no IPv4 exposure is needed through these Services. This configuration does not preserve IPv4 PXE access. Both family settings default to empty, which preserves Kubernetes defaults.
- Keep `port` at its default of `8080` and `alternatePort` at its default of `80`. Setting `alternatePort` to `0` disables the companion Service on port 80.
- Keep `externalTrafficPolicy` at its default of `Local` to preserve client source addresses. Route external traffic through nodes with serving PXE pods.

An existing Service's primary address family, the first entry in `ipFamilies`, cannot change in place. Preserve that family when adding a second one.

Provision an IPv6 address reachable from boot clients through the site's load balancer. Set `nico-dhcp.dhcp.v6ProvisioningServer` to that address. The chart does not automatically copy the Service address into this setting.

For an explicit MetalLB address reservation, set `metallb.universe.tf/loadBalancerIPs` in `nico-pxe.externalService.annotations`. Use an IPv6 VIP from the site's IPv6 pool. For dual-stack exposure, include the retained IPv4 VIP and the IPv6 VIP as a comma-separated string. Omitting the annotation permits automatic address allocation. Configure the matching address pools and advertisement or routing.

The cluster and CNI must provide IPv6 pod addresses and backend endpoints for PXE, and the PXE listener must accept IPv6 connections. An IPv6 Service or VIP alone does not provide IPv6 connectivity to its pods.

Apply the values through the site's Helm upgrade workflow. Provisioning-address changes take effect after the DHCPv6 pod restarts. The configuration checksum triggers that rollout. [HTTP Boot Options](#http-boot-options) describes client eligibility and explicit API URL overrides.

### Follow the Boot Sequence

Firmware and iPXE configure networking separately. The handoff proceeds as follows:

1. UEFI firmware downloads and starts `ipxe.efi` using the HTTP boot URL from DHCPv6 option 59 or a firmware-configured boot option. Firmware exposes that URL in its EFI device path, which iPXE uses as its current working URI (`cwuri`).
2. NICo's embedded script tries DHCPv4 first. If DHCPv4 succeeds, it contacts the PXE API at `http://${next-server}` on port 80. A later HTTP failure does not switch to IPv6.
3. If DHCPv4 fails, the script configures IPv6 and selects the lease in `netX.dhcpv6/ip6` when present. Otherwise, it selects the SLAAC address from Neighbor Discovery Protocol (NDP) settings in `netX.ndp/ip6`. It binds HTTP and HTTPS requests to the selected address and interface through `http-source6`. Core must have registered that exact source address.
4. On the IPv6 path, iPXE requests `/api/v0/pxe/boot` using the firmware-provided URL's scheme, server, and port. The root-relative API path replaces the EFI-file path. It is not appended to the artifact's directory.
5. iPXE executes the returned boot script. That script supplies its own payload URLs. Reusing the firmware's server for the PXE API does not rewrite them.

For example, a firmware URL of `http://[2001:db8:10::10]:8080/public/blobs/internal/x86_64/ipxe.efi` gives iPXE the API endpoint `http://[2001:db8:10::10]:8080/api/v0/pxe/boot`. The machine-information request uses `/api/v0/pxe/whoami` on the same server and port. iPXE does not discover another server when that endpoint is unusable.

Without site configuration or API overrides, the standard PXE template uses `http://carbide-pxe.forge` as its static payload base. That hostname needs an AAAA record and IPv6 reachability on port 80, even when firmware loads iPXE from an IPv6 literal on port 8080.

### Verify the Boot Handoff

Use a representative host through the actual relay path. Confirm each stage before proceeding to the next:

1. Firmware retrieves the intended architecture's EFI artifact and starts NICo's embedded script, which prints the NICo version banner.
2. For stateful boot, confirm that iPXE obtains its own DHCPv6 lease. For SLAAC boot, confirm its options-only INFORMATION-REQUEST completes through the site relay. An options-only reply is not a lease. A successful firmware exchange alone does not prove either iPXE path.
3. On the host, select **iPXE Debug Shell** from the NICo menu and run `show http-source6`. The value has the form `<IPv6-address>%<interface>`. Compare the address before `%` with Core's stored `Address`, using the commands in [Check the Boot Prerequisites](#check-the-boot-prerequisites). For SLAAC inference, expect `Family` to be `IPv6` and `Type` to be `slaac`. A missing setting means no IPv6 HTTP source binding is set, not that the artifact lacks SLAAC support. Run `exit` to return to the menu.
4. The PXE API identifies the interface. The menu's **Print information about this machine** entry displays its stored hostname under the FQDN (fully qualified domain name) label, along with its MAC address. It does not display IP addresses, gateways, or DNS settings.
5. The returned boot script retrieves its payloads and completes the intended operating-system boot.

Firmware configuration and platform qualification are separate from the iPXE handoff, as tracked in [Configure Hosts to HTTP Boot Over IPv6](https://github.com/dsx-ai-factory/infra-controller/issues/7173). Reaching iPXE or the PXE API does not establish a complete Core-managed operating-system boot.

### Diagnose the Embedded Handoff

If IPv6 configuration fails, iPXE exits before checking for an address or boot URL. Check router advertisements and the DHCPv6 exchange through the relay. After configuration succeeds, a missing prerequisite produces one of these diagnostics:

| Message | Check |
| --- | --- |
| "NICo IPv6 boot requires a DHCPv6 lease or SLAAC address." | Neither `netX.dhcpv6/ip6` nor `netX.ndp/ip6` is present on the selected interface. Check the lease or SLAAC configuration. |
| "NICo IPv6 boot requires an inherited HTTP boot URI." | Confirm that firmware passes the HTTP or HTTPS URL used to load `ipxe.efi` in its EFI device path. |

The address diagnostic checks local iPXE settings, not Core registration. An NDP setting alone does not prove usable SLAAC. Non-autonomous or non-`/64` prefixes do not satisfy the router-advertisement requirements in [Check the Boot Prerequisites](#check-the-boot-prerequisites).

An unregistered SLAAC address passes the local check but fails later when the PXE API looks up the source address. That failure returns to the NICo menu and its normal retry flow. It does not automatically return to firmware or select another interface. Repeat the stored-segment and stored-address checks in [Check the Boot Prerequisites](#check-the-boot-prerequisites), and confirm that relay option 79 supplies the client's MAC. Core also rejects inference if another interface already owns the derived address.

Kea caches successful options-only responses for 60 seconds from insertion. Repeated requests can receive the cached reply without a new Core registration attempt; they do not extend that expiry. After correcting address state, repeat [Verify the Boot Handoff](#verify-the-boot-handoff) after the cached response expires. Confirm the stored address after a fresh eligible request reaches Core. Cache expiry alone does not send a request or repair the record.

The diagnostic "NICo IPv6 boot requires a stateful DHCPv6 lease." identifies an artifact without the SLAAC fallback. Follow [Build and Deploy the EFI Artifacts](#build-and-deploy-the-efi-artifacts) using a revision containing the linked SLAAC implementation. The absence of that diagnostic alone does not establish support.

For a fetch failure, check IPv6 reachability, DNS, the boot URL's port, and the root-relative API paths. While HTTP source binding is active, an IPv4-only destination does not trigger IPv4 fallback. This also applies to redirects and payload URLs.

If the returned script reports "Failed to fetch custom_ipxe", inspect the accompanying Core error. For an unknown host interface, confirm that NICo records the client's address and the PXE API receives that same source address. A proxy or NAT that substitutes another source address prevents this lookup. The machine-information errors "Could not load interface" and "Could not load instructions" indicate missing discovery data in the API response.

## Reference

### Helm Values

Set these under `nico-dhcp:` in umbrella chart values, or at the top level for a direct `nico-dhcp` subchart installation.

<ParamField path="config.kea.declineProbationPeriod" type="integer">
Default: `900`. Kea decline quarantine in seconds, shared by both structured configurations. Explicit zero is retained.
</ParamField>

<ParamField path="dhcp.interface" type="string">
Default: `eth0`. Pod interface name used by DHCPv6. The template also supplies this default when the stored value is absent.
</ParamField>

<ParamField path="dhcp.v6DnsServers" type="string">
Default: empty. Comma-separated IPv6 literals for DNS resolvers, emitted in DHCPv6 option 23 in input order. Whitespace is trimmed, empty entries are skipped, and duplicates are retained. An empty value omits the option.
</ParamField>

<ParamField path="dhcp.v6Enabled" type="boolean">
Default: `false`. Creates the IPv6 Deployment, ConfigMap, and data and metrics Services only when true. Absent maps in stored pre-IPv6 values leave IPv6 disabled.
</ParamField>

<ParamField path="dhcp.v6NtpServers" type="string">
Default: empty. Comma-separated IPv6 literals, emitted in option 56 as Network Time Protocol (NTP) servers in input order. Whitespace is trimmed, empty entries are skipped, and duplicates are retained. An empty value omits the option.
</ParamField>

<ParamField path="dhcp.v6ProvisioningServer" type="string">
Default: empty. Optional IPv6 literal for HTTP boot URLs generated by the DHCPv6 hook. Omission or an empty value disables URL generation. [Configure the Boot Server](#configure-the-boot-server) describes deployment requirements, and [HTTP Boot Options](#http-boot-options) describes client conditions and reply options.
</ParamField>

<ParamField path="dhcp.v6RapidCommit" type="boolean">
Default: `false`. A SOLICIT gets a single REPLY only when the client also requests Rapid Commit. Otherwise, use SOLICIT, ADVERTISE, REQUEST, and REPLY.
</ParamField>

<ParamField path="dhcp.v6ServerIdentifier" type="string" required>
A quoted string of exactly 12 hexadecimal digits, with no separators. Required when DHCPv6 is enabled.
</ParamField>

<ParamField path="metrics.port" type="integer | string">
Default: `1089`. Integer or quoted decimal string without leading zeros, from 1 through 65535. Controls both metrics Services and the DHCPv6 hook listener. Keep the DHCPv4 hook endpoint aligned for Service scraping.
</ParamField>

<ParamField path="v6ExternalService.annotations" type="map">
Default: empty map. Service annotations. Set `metallb.universe.tf/loadBalancerIPs` to the IPv6 VIP when using MetalLB. Disabled Services can retain blank site placeholders.
</ParamField>

<ParamField path="v6ExternalService.enabled" type="boolean">
Default: `false`. Adds the IPv6 LoadBalancer Service when DHCPv6 is also enabled. An absent map is disabled.
</ParamField>

The four v6 hook values render as `hook-dns-servers-ipv6`, `hook-ntp-servers-ipv6`, `hook-provisioning-server-ipv6`, and `hook-rapid-commit-v6`. Invalid IPv6 literals fail hook initialization. Helm rejects an invalid server identifier, non-Boolean Rapid Commit, and invalid metrics port types or bounds before startup.

DHCPv6 reuses `config.kea.hookParameters.nicoApiUrl`. An empty value derives the API URL from `apiServiceName` and `namespaceOverride`, falling back to the release namespace when `namespaceOverride` is empty. Its metrics listener binds `[::]:<metrics.port>`. DHCPv4 structured configuration preserves the complete address and port from `config.kea.hookParameters.nicoMetricsEndpoint`. Set its port to match `metrics.port` for Service scraping. The v4 `config.enabled` and `config.keaConfigJsonRaw` controls apply only to the v4 ConfigMap. A raw or externally managed v4 configuration must itself match the metrics Service port.

### HTTP Boot Options

[Check the Boot Prerequisites](#check-the-boot-prerequisites) and [Configure the Boot Server](#configure-the-boot-server) describe the required artifacts, firmware, and server configuration.

The DHCPv6 hook adds boot options only for SOLICIT, REQUEST, RENEW, REBIND, and INFORMATION-REQUEST, not CONFIRM, RELEASE, or DECLINE. Stateful replies that refuse address allocation receive no hook-added boot options. Options-only requests remain eligible.

When the hook adds option 59, 16, or 61, it replaces the corresponding native Kea `option-data` value. If the hook does not add one of these options, it leaves any native value for that option unchanged.

For either URL source, the client's option request option (ORO) must include boot-file URL option 59. Otherwise, the hook adds no boot options. The hook accepts an explicit URL in the API's optional `DhcpRecord.booturl` field. Site DHCP discovery leaves this field unset. It is not a Helm setting. URL selection follows these rules:

1. A nonempty API `booturl` is returned unchanged, even without a provisioning address, HTTP vendor class, or supported architecture.
2. An explicitly empty API `booturl` disables hook-added boot options without selecting a generated default.
3. When API `booturl` is absent, generation requires a configured provisioning address and an HTTP boot client with a supported architecture.

For generation, the client's vendor-class option 16 must include a data entry exactly equal to `HTTPClient` or beginning with `HTTPClient:`. Matching is case-sensitive. The hook selects the first supported HTTP boot architecture from client system architecture type option 61, using the client's preference order:

- Architecture code 16 selects the `x86_64` directory.
- Architecture code 19 selects the `aarch64` directory.

Other architecture codes are skipped. Without a supported architecture in option 61, the hook does not generate a URL. Architecture text in the vendor-class string does not replace option 61.

For a generated URL, the hook also adds option 61 with only the selected architecture, whether or not the ORO requests option 61. It adds no architecture option for an explicit API `booturl`.

The hook adds the `HTTPClient` marker in option 16 only when it returns option 59 and all of these conditions hold:

- The ORO requests option 16.
- The client supplies an HTTP vendor class as described above.
- The selected URL uses HTTP or HTTPS.
- The URL host is an IPv6 literal, or IPv6 DNS servers are configured through `dhcp.v6DnsServers` or `hook-dns-servers-ipv6`.

The reply preserves the enterprise number from the matching request vendor class and returns the bare `HTTPClient` value. With a hostname or IPv4-literal URL and no IPv6 DNS, the hook still adds option 59 but omits the `HTTPClient` marker. This preserves compatibility with EFI Development Kit II (EDK2) firmware, which can reject a marked offer before trying a firmware-configured URL without IPv6 DNS.

### Interface Selection

The supplied `ipxe.efi` uses the firmware's boot network device (`snponly`). Native-driver variants such as `golan.efi` can expose multiple supported interfaces. The following selection rule applies to interfaces visible to the chosen build.

iPXE's IPv6 configuration command stops at the first successfully configured interface. NICo's embedded script selects its DHCPv6 lease or NDP SLAAC address only after that selection. It does not try another interface if the selected interface has neither address, or if Core has not registered its source address.

Provide a NICo lease or a registered modified EUI-64 SLAAC address on the interface iPXE selects. Continuing to another suitable interface requires an embedded-script change, not a DHCP server setting.

### Custom Scripts and Network Reconfiguration

The `http-source6` setting binds HTTP and HTTPS requests to an IPv6 address and interface. On the IPv6 boot path, NICo sets it to the DHCPv6 lease or, if no lease is present, the NDP SLAAC address. Network commands in a custom script do not clear this setting automatically.

To rerun NICo's network configuration, press p at the boot or retry prompt and choose **Boot according to NICo workflow**. This action clears `http-source6`, tries DHCPv4, and establishes a new binding if it selects IPv6. **Print information about this machine** also reruns network configuration. **Retry boot** returns to the startup prompt, which runs the workflow after a four-second timeout. An idle menu selects the workflow after five seconds. Entering the menu or **iPXE Debug Shell** does not itself clear the binding.

A custom script can run `clear http-source6` before payload downloads that need normal address-family and source-address selection. Clearing the setting does not configure IPv4 or add routes. Keep or restore an address that NICo recognizes before making further PXE API requests.

On either address-family path, iPXE retries a failed one-time custom PXE script if the response marks it retryable. It waits 30 seconds and executes the cached script again without another `/api/v0/pxe/boot` request. A cached retry does not rerun network configuration, so it retains any `http-source6` change made by your script. Retries continue until execution succeeds or you press p during the retry prompt to return to the menu. Payload downloads can repeat.

Starting the NICo workflow again, including through a menu timeout, discards that cached script and sends a new boot request. Core consumes a one-time request when it supplies the script, not when the script finishes successfully. Submit another one-time request if you need the new workflow to fetch it again.

### Implementation References

The following references identify the source changes and their tracked scope:

- [DHCP/06: Deploy Kea DHCPv6, Metrics, and Helm](https://github.com/dsx-ai-factory/infra-controller/issues/2388) tracks the opt-in service deployment.
- [feat(dhcp): return HTTP boot URLs from the site DHCPv6 server](https://github.com/dsx-ai-factory/infra-controller/pull/7574) implements [Return HTTP Boot URLs From the Site DHCPv6 Server](https://github.com/dsx-ai-factory/infra-controller/issues/7171).
- [feat(ipxe): boot from IPv6-only provisioning networks](https://github.com/dsx-ai-factory/infra-controller/pull/7650) implements [Boot Through the Embedded iPXE Script on IPv6-Only Networks](https://github.com/dsx-ai-factory/infra-controller/issues/7172).
- [feat(ipxe): support SLAAC boot through site DHCPv6 relays](https://github.com/dsx-ai-factory/infra-controller/pull/8290) implements [Support iPXE SLAAC Boot Through Site DHCPv6 Relays](https://github.com/dsx-ai-factory/infra-controller/issues/8279).
