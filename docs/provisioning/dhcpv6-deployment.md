# Deploy DHCPv6 <Badge intent="info">v2.2</Badge> <Badge intent="launch" minimal>New</Badge>

Dynamic Host Configuration Protocol for IPv6 (DHCPv6) is opt-in and has not been deployed to production. The deployment work is tracked by [DHCP06](https://github.com/NVIDIA/infra-controller/issues/2388). Validate the target pod network and relay path before enabling it.

The Core chart runs Kea DHCPv4 in `nico-dhcp` and, when enabled, Kea DHCPv6 in `nico-dhcp6`. The Deployments have separate pods, selectors, configuration, Services, probes, and rollouts. Both use the same image, hook library, ServiceAccount, client certificate, and application programming interface (API) identity. Both hooks call the same API through gRPC with mutual Transport Layer Security (mTLS). API role-based access control (RBAC) continues to apply.

## Network and Identity Requirements

- The DHCPv6 pod needs a routable IPv6 address on its listening interface and reachability to the API. An IPv6 Service alone does not assign an IPv6 address to a pod. Configure the cluster's Container Network Interface (CNI) plugin and eligible nodes accordingly.
- Supply a site-unique, stable server identifier consisting of exactly 12 hexadecimal digits. Keep it unchanged across pod replacements and upgrades. Kea advertises a DHCP unique identifier (DUID) based on a link-layer address (DUID-LL), with Ethernet hardware type and this identifier. It does not generate or persist a pod-local identity.
- Point IPv4 relays to the IPv4-only DHCP Service on UDP 67. Point IPv6 relays to the separate IPv6-only DHCP Service on UDP 547. A MetalLB deployment needs a distinct IPv6 virtual IP address (VIP) from an IPv6 address pool and a working route from the relay to that VIP.
- Configure relays to supply RFC 6939 Client Link-Layer Address option 79 for clients whose DUID has no Ethernet media access control (MAC) address. These include enterprise-number DUIDs (DUID-EN) and universally unique identifier DUIDs (DUID-UUID).
- [Prepare the managed network](#prepare-the-managed-network) with an IPv6 prefix and the relay link-address before enabling DHCPv6.
- DHCPv6 runs one replica with a `Recreate` rollout. The chart's `replicas` value controls DHCPv4 only and defaults to one. Enabling DHCPv6 does not restrict that setting. Leases use pod-local memfiles. Durable lease storage and active-writer coordination remain outside this deployment's supported scope.

NICo accepts option 79 only from relay metadata, with Ethernet hardware type 1 and exactly six MAC bytes. That MAC takes precedence over a DUID-LL or DUID-LLT (link-layer address plus time) MAC. Mismatches produce a warning. If the relay supplies no usable option 79, NICo falls back to the Ethernet MAC in DUID-LL or DUID-LLT. Requests without either MAC source are dropped before API discovery (`no_mac_no_option79`). A missing or malformed client DUID is rejected even when option 79 is usable.

Startup selects the first global-scope IPv6 address on the configured pod interface and binds Kea to that address explicitly. Use an interface with one routable pod IPv6 address. A missing address stops startup. Startup and readiness probes also require a bound IPv6 UDP 547 socket. Readiness checks the hook endpoint, and liveness checks the metrics HTTP server. These probes do not prove off-host routing or API allocation. Validate those with a representative client through the actual relay.

The IPv6 LoadBalancer Service uses `externalTrafficPolicy: Local` to preserve relay source addresses and ports. Route or advertise its VIP only through nodes with a ready DHCPv6 pod. With one replica, only its node can serve external requests. Any upstream load balancer must also preserve the relay's source address and UDP port. Source network address translation (NAT) that changes UDP 547 can prevent Kea's reply from returning to the relay.

## Prepare the Managed Network

For a new segment, add the following definition to the existing TOML in `nico-api.siteConfig.nicoApiSiteConfig` (Helm), or `deploy/files/nico-api/nico-api-site-config.toml` (Kustomize). Retain the rest of the site's TOML. A Helm string override replaces the entire value. Replace the segment name and addresses with unused site allocations before the segment is first created:

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

Apply the site configuration through the normal API deployment workflow, then confirm the stored segment contains both prefixes before enabling the DHCPv6 workload. Network definitions are applied only when the segment is first created. Adding `prefix_v6` or changing `dhcpv6_link_address` on an existing seeded segment only reports configuration drift. It does not update the stored prefixes. The network-segment command-line interface (CLI) and remote procedure calls (RPCs) provide no in-place prefix update. Existing IPv4 segments therefore need a reviewed segment migration before DHCPv6 can be enabled for their clients. Keep the DHCPv6 workload disabled until the segments it will serve are ready.

## Helm Values

Set these under `nico-dhcp:` in umbrella chart values, or at the top level for a direct `nico-dhcp` subchart installation.

<ParamField path="config.kea.declineProbationPeriod" type="integer">
Default: `900`. Kea decline quarantine in seconds, shared by both structured configurations. Explicit zero is retained.
</ParamField>

<ParamField path="dhcp.interface" type="string">
Default: `eth0`. Pod interface name used by DHCPv6. The template also supplies this default when the stored value is absent.
</ParamField>

<ParamField path="dhcp.v6DnsServers" type="string">
Default: empty. Comma-separated IPv6 literals for Domain Name System (DNS) resolvers, emitted in DHCPv6 option 23 in input order. Whitespace is trimmed, empty entries are skipped, and duplicates are retained. An empty value omits the option.
</ParamField>

<ParamField path="dhcp.v6Enabled" type="boolean">
Default: `false`. Creates the IPv6 Deployment, ConfigMap, and data and metrics Services only when true. Absent maps in stored pre-IPv6 values leave IPv6 disabled.
</ParamField>

<ParamField path="dhcp.v6NtpServers" type="string">
Default: empty. Comma-separated IPv6 literals, emitted in option 56 as Network Time Protocol (NTP) servers in input order. Whitespace is trimmed, empty entries are skipped, and duplicates are retained. An empty value omits the option.
</ParamField>

<ParamField path="dhcp.v6ProvisioningServer" type="string">
Default: empty. Optional IPv6 literal for HTTP boot URLs generated by the DHCPv6 hook. Omission or an empty value disables URL generation. [HTTP Boot Options](#http-boot-options) describes image requirements, client conditions, and reply options.
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

For a site whose IPv6 networking is ready, an enablement values file can contain:

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

Apply through the site's normal Helm upgrade workflow. Stored values from a chart without the `dhcp` and `v6ExternalService` maps are supported with `--reuse-values`. A chart-only upgrade preserves an older pinned Core image's IPv4 listener defaults. DSX, hardware-health, and PXE adopt their dual-stack defaults when the Core image is upgraded, unless an explicit listener configuration overrides them. Setting `dhcp.v6Enabled: false` removes the v6 resources while preserving the v4 pod template. The v6 Deployment uses `Recreate` so old and replacement v6 pods do not serve simultaneously.

## Kustomize

The documented root, `deploy/kustomization.yaml`, generates both DHCP ConfigMaps in `nico-system`. IPv6 stays inactive until `components/dhcp6` is added to that root's `components` list.

Before enabling the component, replace `{{ NICO_DHCPV6_SERVER_IDENTIFIER }}` in `deploy/files/kea-dhcp6-carbide.conf` and `{{ NICO_DHCPV6_EXTERNAL_IP }}` in `deploy/components/dhcp6/service.yaml`. Set the DNS and NTP hook strings in the IPv6 config as needed. For HTTP boot, set `hook-provisioning-server-ipv6` as described in [HTTP Boot Options](#http-boot-options). These strings default to empty. The component uses `eth0`, port 1089, and a 900-second decline quarantine. Like the raw IPv4 Deployment, it inherits the namespace's default ServiceAccount and mounts `nico-dhcp-certificate`. To change the interface, update both the config's interface prefix and the Deployment's `DHCP_INTERFACE` environment value. To change the metrics port, update the hook endpoint, Deployment port/readiness URL, and metrics Service together. The `__NICO_DHCPV6_ADDRESS__` token is resolved at pod startup.

Both raw configurations use the portable image path `/usr/lib/kea/hooks/libdhcp.so`. Before applying this configuration with a pinned older or custom image, verify that it provides this path. The DHCPv4 ConfigMap changes even with DHCPv6 disabled. An active Reloader watching it can restart DHCPv4. The IPv6 pod references the `imagepullsecret` registry Secret in `nico-system`, matching the IPv4 workload. Supply the complete root's [site files and rendering prerequisites](https://github.com/NVIDIA/infra-controller/blob/main/deploy/README.md#files-inputs-deployfiles), including the encrypted SSH host-key Secret, its decryption credentials, and Unbound's base forwarders file. With standalone `kustomize` and `ksops` on `PATH`, render from the repository root before applying it:

```bash
kustomize build deploy --enable-alpha-plugins --enable-exec
```

## HTTP Boot Options

HTTP boot option support is unreleased and is not included in NICo v2.2.1. For source builds, use a Core image containing [the implementation in PR #7574](https://github.com/dsx-ai-factory/infra-controller/pull/7574). This implements [site DHCPv6 HTTP boot support](https://github.com/dsx-ai-factory/infra-controller/issues/7171).

Set `dhcp.v6ProvisioningServer` to the provisioning server's IPv6 address, without brackets, a port, a CIDR suffix, or surrounding whitespace. An empty or whitespace-only value disables generated URLs. Other values that are not IPv6 literals fail hook initialization.

Provisioning-address changes take effect after the DHCPv6 pod restarts. Helm upgrades trigger a rollout through the configuration checksum. Kustomize copies configuration at startup. After updating the ConfigMap, an active Stakater Reloader watching the DHCPv6 Deployment restarts it. Otherwise, restart the Deployment manually.

The following rules control options added by the hook. The hook adds boot options only for SOLICIT, REQUEST, RENEW, REBIND, and INFORMATION-REQUEST, not CONFIRM, RELEASE, or DECLINE. Stateful replies that refuse address allocation receive no hook-added boot options. Options-only requests remain eligible.

When the hook adds option 59, 16, or 61, it replaces the corresponding native Kea `option-data` value. If the hook does not add one of these options, it leaves any native value for that option unchanged.

For either URL source, the client's option request option (ORO) must include boot-file URL option 59. Otherwise, the hook adds no boot options. The hook accepts an explicit URL in the API's optional `DhcpRecord.booturl` field. Site DHCP discovery leaves this field unset. It is not a Helm setting. URL selection follows these rules:

1. A nonempty API `booturl` is returned unchanged, even without a provisioning address, HTTP vendor class, or supported architecture.
2. An explicitly empty API `booturl` disables hook-added boot options without selecting a generated default.
3. When API `booturl` is absent, generation requires a configured provisioning address and an HTTP boot client with a supported architecture.

For generation, the client's vendor-class option 16 must include a data entry exactly equal to `HTTPClient` or beginning with `HTTPClient:`. Matching is case-sensitive. The hook selects the first supported HTTP boot architecture from client system architecture type option 61, using the client's preference order:

- Architecture code 16 selects the `x86_64` directory.
- Architecture code 19 selects the `aarch64` directory.

Other architecture codes are skipped. Without a supported architecture in option 61, the hook does not generate a URL. Architecture text in the vendor-class string does not replace option 61. The generated URL uses this format:

```text
http://[<IPv6-address>]:8080/public/blobs/internal/<architecture>/ipxe.efi
```

The server must expose this path over IPv6 HTTP on TCP port 8080 to boot clients, including `ipxe.efi` for each required architecture. For the bundled `nico-pxe` external Service, configure the following settings under `nico-pxe.externalService` in umbrella chart values:

- Set `enabled` to `true`. The default is `false`.
- Set `ipFamilyPolicy` to `SingleStack` with `ipFamilies: [IPv6]`, or to `RequireDualStack` with `ipFamilies: [IPv4, IPv6]`. Both settings default to empty, which preserves Kubernetes defaults.
- Keep `port` at its default of `8080`.

An existing Service's primary address family, the first entry in `ipFamilies`, cannot change in place. Preserve that family when adding a second one.

For a generated URL, the hook also adds option 61 with only the selected architecture, whether or not the ORO requests option 61. It adds no architecture option for an explicit API `booturl`.

The hook adds the `HTTPClient` marker in option 16 only when it returns option 59 and all of these conditions hold:

- The ORO requests option 16.
- The client supplies an HTTP vendor class as described above.
- The selected URL uses HTTP or HTTPS.
- The URL host is an IPv6 literal, or IPv6 DNS servers are configured through `dhcp.v6DnsServers` or `hook-dns-servers-ipv6`.

The reply preserves the enterprise number from the matching request vendor class and returns the bare `HTTPClient` value. With a hostname or IPv4-literal URL and no IPv6 DNS, the hook still adds option 59 but omits the `HTTPClient` marker. This preserves compatibility with EFI Development Kit II (EDK2) firmware, which can reject a marked offer before trying a firmware-configured URL without IPv6 DNS.

Finishing HTTP boot also requires [firmware support for HTTP boot over IPv6](https://github.com/dsx-ai-factory/infra-controller/issues/7173) and an [embedded iPXE script that works without DHCPv4](https://github.com/dsx-ai-factory/infra-controller/issues/7172).

## Deployment Verification and Metrics

Validate address allocation with a representative DHCPv6 client through the actual relay and confirm that it receives an address from the managed IPv6 prefix. Before serving DUID-EN or DUID-UUID clients, verify that the relay supplies a usable option 79 with a representative client of that type.

Both independent DHCP metrics targets use the configured port (1089 by default). With ServiceMonitor enabled, the common `app.kubernetes.io/metrics` label discovers both through the named `http` port. Check both pod targets after the exchange:

```bash
curl --noproxy '*' -fsS 'http://192.0.2.10:1089/metrics'
curl --noproxy '*' -fsS 'http://[2001:db8:10::10]:1089/metrics'
```

Replace the example addresses with the actual v4 and v6 pod addresses. Confirm that the v6 request count increased and that the v4 endpoint remains independently reachable. [NICo Metrics](../observability/metrics.md) describes the metrics Service address-family policy and shared TCP listener behavior.
