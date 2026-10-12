# DNS <Badge intent="info">v2.0</Badge>

NICo publishes DNS records for eligible machine, BMC, and instance addresses in the `nico-api` database. Inventory changes update these records automatically. This page covers the published names, infrastructure forward zones, record lifetimes, and reverse (PTR) lookups. For the `nico-dns` deployment, the recursive resolver in front of it, and the separately configured infrastructure service names, refer to [IP and Network Configuration](../provisioning/ip-and-network-configuration.md#3-dns-configuration).

## What Gets a Name

Every managed machine interface gets a hostname; [Host Naming](host-naming.md) covers how that label is chosen. These are the forward records NICo serves:

| Name | Answers for | Notes |
|---|---|---|
| `<hostname>.<domain>` | A host's primary interface and every BMC interface | The human-facing name, built by the host-naming strategy |
| `<machine-id>.adm.<domain>` | A host's primary interface | Keyed on the stable machine id |
| `<machine-id>.bmc.<domain>` | A machine's BMC interface | Host, predicted-host, and DPU machine ids |
| `<ip-hostname>.<segment subdomain>` | An overlay (DPU-managed) instance address | Always IP-derived (`10-1-2-3`, or the fully expanded IPv6 form), independent of the host-naming strategy |

Records serve as A or AAAA per address family, with a 300-second TTL unless the owning domain sets `default_ttl`. An interface that loses all of its addresses drops out of DNS until it has one again. Instances on `host_inband` segments publish no instance record: their address is the host's own interface address, which the machine records above already serve.

## The Site Zone and Segment Subdomains

The site's forward zone is seeded once, at first startup, from the `nico-api` config:

```toml
initial_domain_name = "mysite.example.com"
```

The seed is inserted only when the domains table is empty, including deleted rows. Changing `initial_domain_name` does not rename an existing domain. At startup, initial networks use the configured infrastructure forward domain if it exists, or the sole infrastructure forward domain. Reverse domains and VPC-owned domains are excluded from this selection. With no suitable domain or multiple fallback candidates, startup logs a warning and skips initial network creation.

Network segments determine the domain used by their interfaces and instances:

- Segments seeded from the config (`[networks.*]`) inherit the site domain automatically.
- Segments created later through the API or CLI name their subdomain explicitly: `nico-admin-cli network-segment create ... --subdomain-id <domain-id>` (required for `host-inband` segments; list domains with `nico-admin-cli domain show`).
- A segment with no subdomain produces no DNS at all: its interfaces and instances have no zone to live in.

### Domain Ownership and Publication

The domain API accepts infrastructure domains and domains owned by a live VPC. Domain names must be lowercase valid DNS names. Live names are globally unique, ignoring case and a trailing root dot, and each VPC can own at most one live domain. Updates cannot rename a domain or change its VPC owner. Delete the domain before deleting its owning VPC.

VPC ownership is registration only. A VPC-owned domain does not establish public DNS authority or publish VPC records, and a network segment cannot use it as its `subdomain_id`, even within that VPC. An enclosing infrastructure zone can still answer queries beneath the registered name. Overlay instance records in infrastructure domains continue to use the publication rules above.

### Record TTL and SOA Serials

The domain's optional `default_ttl` applies to its A, AAAA, and PTR records. Domain creation and updates accept 30 through 86,400 seconds, inclusive. Omission at creation uses the 300-second default. Omission on update preserves the stored value; the update API cannot clear an explicit value.

The SOA has separate lifetime fields. A newly created domain has an SOA record TTL of 3,600 seconds and an SOA minimum of 120 seconds. Changing `default_ttl` does not change either field. Forward-zone SOA serials advance with domain updates and inventory changes that affect published records. Serial tracking does not enable zone transfers or invalidate cached answers.

## Reverse DNS

Reverse resolution mirrors the forward records that matter to humans:

- An address on a primary interface or a BMC interface answers PTR with `<hostname>.<domain>`.
- An overlay instance address answers PTR with its instance name.
- The `adm` and `bmc` machine-id forms are forward-only.

PTR lookup uses the complete IPv4 or IPv6 address encoded in the reverse query name. It does not require an octet-aligned or nibble-aligned network prefix, or a stored reverse zone. A PTR is returned only when exactly one inventory owner holds the address and that owner has a publishable name. Multiple owners suppress the answer, even if only one has DNS metadata. Retained overlay addresses can count as owners during segment deletion until their address rows are removed.

Published PTR responses set the authoritative-answer (AA) bit. They do not establish authority over the enclosing `in-addr.arpa` or `ip6.arpa` zone. Missing, ambiguous, or incomplete PTR queries return REFUSED without an SOA or AA bit. Supported non-PTR queries in either reverse tree, including SOA and NS, also return REFUSED.

Configure the site's recursive resolver to forward the required reverse lookups to `nico-dns` if clients need these PTR answers. NICo does not serve authoritative reverse zones, reverse SOAs, or reverse-zone delegation records. Explicit reverse-domain creation is rejected. Network lifecycle code retains reverse-domain rows for rollback compatibility, but those rows do not enable reverse authority.

## DNS Process Configuration

The `carbide-dns` binary reads an optional TOML file selected with the `run` subcommand's `--config-file` option. There is no implicit configuration-file path. Omitted keys use built-in defaults; unknown keys are ignored. An unreadable file or invalid TOML prevents startup. Explicit CLI options override the file.

The following TOML settings control caching and API lookup timeouts. All are optional, and none has a CLI override:

| Key | Default | Behavior |
|---|---|---|
| `negative_cache_ttl_secs` | `120` | Maximum non-transient negative lifetime in seconds; zero makes these entries expire immediately |
| `negative_cache_servfail_ttl_secs` | `5` | Transient failure lifetime in seconds, clamped to 1 through 300 |
| `negative_cache_entries_max_count` | `50000` | Maximum cache entries; inserting a new key at capacity evicts the least-recently-used entry; zero selects a capacity of one |
| `upstream_lookup_timeout_secs` | `5` | Maximum duration of an API record lookup in seconds before returning SERVFAIL |

The stock Helm chart does not mount a DNS TOML file or pass `--config-file`, so these settings retain their built-in defaults. Its `listenAddress` value supplies the DNS listener flag, defaulting to `[::]:53` for UDP and TCP. The chart also passes the API URI from the `NICO_API` ConfigMap key as a command-line argument, overriding the binary's default of `https://nico-api.nico-system.svc.cluster.local:1079`.

The binary exposes `/metrics`, `/health`, and `/ready` on `metrics_listen_address`, default `[::]:8053`. The health and readiness endpoints do not perform a DNS query or an ongoing API connectivity check. Verify actual DNS responses separately. Trace export is disabled unless an OTLP endpoint is configured; `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` takes precedence over the CLI or TOML endpoint.

## Resolvers Handed to Hosts

DHCP option 6 tells managed machines where to resolve, and it must point at the recursive resolver, never at `nico-dns` directly (`nico-dns` cannot answer external names or the infrastructure service names):

- On the site DHCP path, option 6 comes from the `nico-dhcp` Kea hook parameter `carbide-nameservers` (the `config.kea.hookParameters.nameservers` Helm value emits it).
- Hosts behind a DPU receive the DPU's own resolver set.
- The opt-in [DHCPv6 deployment](../provisioning/dhcpv6-deployment.md) advertises IPv6 recursive resolvers in option 23 through the `dhcp.v6DnsServers` Helm value.

## Unbound IPv6 Transport

The combined `nico-unbound` Service serves DNS on UDP/TCP 53 and, when `exporter.enabled` is true (chart default), exporter metrics on TCP 9167. `unbound.ipv6.enabled` is an optional Boolean with chart default `false`. An omitted or null map, or an omitted flag, also leaves the Service IPv4-only. `true` requests `PreferDualStack` while retaining IPv4 as the primary family. The flag controls this Service's exposure only. It does not change the image, startup command, mounted configuration, exporter, or the separate external DNS Services.

You supply the Unbound and exporter images. IPv6 enablement requires that Unbound consumes the configuration mounted at `/etc/unbound/local.conf.d`, listens on IPv6 UDP and TCP 53, retains its existing IPv4 listeners, and permits the intended IPv6 clients. The exporter must separately listen on IPv6 TCP 9167 when enabled. Both containers need the pod's working IPv6 network. Image tags alone do not establish these capabilities.

Use the image's existing configuration mechanism. For an image that includes the chart's fragments and has no listener directives elsewhere, the following umbrella-chart values illustrate the complete DNS listener and access configuration. Replace the IPv6 prefix with the source network Unbound will actually see. Preserve your site's existing IPv4 listener addresses and access rules. The example repeats the chart's default IPv4 access rule. Omit listener directives already present elsewhere in the image's effective configuration, since explicit interfaces accumulate. When installing the Unbound chart directly, omit the outer `unbound:` key.

```yaml
unbound:
  ipv6:
    enabled: true
  localConfig:
    access_control.conf: |
      server:
          do-ip4: yes
          do-ip6: yes
          interface: 0.0.0.0
          interface: ::
          access-control: 0.0.0.0/0 allow
          access-control: 2001:db8:10::/64 allow
```

This string replaces the existing `access_control.conf` value. Merge all required site access rules into it, including any source addresses introduced by network address translation (NAT). `do-ip6: yes` also permits outbound IPv6 DNS traffic. You can still use existing IPv4 forwarders. Apply listener configuration first, restart the resolver through the site's normal rollout workflow, and verify it before enabling the Service's IPv6 exposure. Interface changes require a restart. A configuration reload alone is insufficient. The [Unbound configuration reference](https://unbound.docs.nlnetlabs.nl/en/latest/manpages/unbound.conf.html) describes listener and access-control semantics.

For Kustomize, configure the image through the existing `deploy/nico-unbound-base/local.conf.d/access_control.conf` and change only `nico-unbound`'s Service `ipFamilyPolicy` to `PreferDualStack` in the site overlay, retaining its IPv4 primary family and all three ports. The base remains IPv4-only. The root deployment publishes this combined Service through a LoadBalancer, so IPv6 enablement also requires compatible virtual IP address (VIP) allocation and routing there. For Helm, configure the separate [Unbound External Services](#unbound-external-services). An internal dual-stack Service does not provide an external IPv6 resolver VIP for bare-metal hosts.

Before advertising an IPv6 resolver to clients, validate the deployed images from an intended client network:

- Query a configured local name and a name resolved through the existing forwarders over IPv6 UDP and TCP 53. Verify the expected DNS answers. Check the pod first, then the Service's IPv6 address after enabling exposure.
- Repeat representative DNS queries over IPv4 and confirm the existing IPv4 Service address is retained.
- Fetch the exporter's metrics through IPv6 and the existing `nico-unbound:9167` IPv4 endpoint when the exporter is enabled.
- Confirm an IPv4-only installation still upgrades with its existing values. The default-off flag introduces no listener configuration or pod-template change.

DNS transport and record type are independent: either IPv4 or IPv6 transport can carry A or AAAA queries. A TCP readiness connection proves neither DNS answers nor IPv6 Service routing. External IPv6 DNS additionally requires a reachable resolver VIP and advertising it to clients through the site's DHCPv6 configuration.

## Unbound External Services

The Unbound chart disables external Services by default (`externalService.enabled: false`). Enabling them creates separate UDP and TCP Services on port 53, with `LoadBalancer` as the default type. Both Services use the same address-family settings and VIP annotations. They expose recursive DNS, not exporter metrics.

Set `unbound.externalService.ipFamilies` and `unbound.externalService.ipFamilyPolicy` in the umbrella-chart values to select IPv4, IPv6, or both. Their chart defaults are `[IPv4]` and `SingleStack`. An omitted, null, or empty setting uses its chart default; an empty list does not select the cluster's default family. These settings do not change the combined internal Service or its independent `unbound.ipv6.enabled` flag.

For IPv6-only external DNS Services, use the following values. Replace the example VIP with an address from your MetalLB pool. When installing the Unbound chart directly, omit the outer `unbound:` key and its `enabled` field.

```yaml
unbound:
  enabled: true
  externalService:
    enabled: true
    ipFamilyPolicy: SingleStack
    ipFamilies: [IPv6]
    annotations:
      metallb.io/loadBalancerIPs: "2001:db8:10::53"
```

For dual-stack external DNS Services with IPv4 primary, use the following values. `RequireDualStack` requires both address families; Kubernetes rejects the Service if the cluster does not support both.

```yaml
unbound:
  enabled: true
  externalService:
    enabled: true
    ipFamilyPolicy: RequireDualStack
    ipFamilies: [IPv4, IPv6]
    annotations:
      metallb.io/loadBalancerIPs: "192.0.2.53,2001:db8:10::53"
```

The `ipFamilies` list contains unique `IPv4` or `IPv6` entries. `SingleStack` accepts one family; a dual-stack list accepts either order. The first entry is primary. Kubernetes permits adding or removing a secondary family but rejects an in-place primary-family change, causing `helm upgrade` to fail. Plan Service replacement and an interruption to resolver access when changing the primary family, including a change from IPv4-only to IPv6-only. Refer to [Kubernetes IPv4/IPv6 Dual-Stack](https://kubernetes.io/docs/concepts/services-networking/dual-stack/#services) for policy and update rules.

With `PreferDualStack` and one preferred family in `ipFamilies`, Kubernetes adds the other family on a dual-stack cluster. On a single-stack cluster, it uses only the listed family, which the cluster must support. Omit pinned VIP annotations to let MetalLB allocate addresses for the resulting Service families, or supply VIPs that match those families.

The chart adds a shared-IP annotation so the UDP and TCP Services can share the configured VIPs. A pinned dual-stack configuration needs one VIP per family and a MetalLB pool containing both families. Refer to [MetalLB IPv6 and Dual-Stack Services](https://metallb.io/usage/#ipv6-and-dual-stack-services). Setup preflight checks supplied VIPs against configured pools and rejects a VIP from the wrong family for `SingleStack`. Omitting VIP annotations permits automatic allocation.

Configure and verify the [Unbound IPv6 Transport](#unbound-ipv6-transport) listeners and access rules before enabling external IPv6 exposure. These Service settings do not configure pod networking, DNS listeners, client access, or routing. Validate DNS answers over UDP and TCP from the intended client network through each enabled VIP family before advertising the resolver.

## Troubleshooting

The following table lists common situations and where to look:

| Symptom | Likely cause / action |
|---|---|
| A machine interface has no name | Only primary and BMC interfaces publish records; the interface can also be addressless, or its segment can lack a subdomain. `nico-admin-cli managed-host show <machine-id>` shows interfaces and the primary flag; the `/admin/ipam/dns` web page lists every record NICo is serving. |
| An instance has no name | The segment's `--subdomain-id` is unset, the address predates instance-hostname population (records populate for addresses allocated going forward), or the instance is on a `host_inband` segment (the host's machine record serves that address). |
| PTR is missing while the forward name resolves | Only the `<hostname>.<domain>` and instance forms answer PTR; the `adm` and `bmc` machine-id forms are forward-only. Check for another inventory owner of the address, including retained overlay addresses during cleanup. Ambiguous ownership returns REFUSED. |
| A registered VPC domain does not resolve | VPC ownership does not publish records or establish public DNS authority. Network segments must use infrastructure domains for the record publication described here. |
| Hosts cannot resolve external names | DHCP option 6 points at `nico-dns` (authoritative only) instead of the recursive resolver. Refer to [IP and Network Configuration](../provisioning/ip-and-network-configuration.md#32-unbound-recursive-resolver-for-managed-machines). |
