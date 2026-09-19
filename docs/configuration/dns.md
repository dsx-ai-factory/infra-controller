# DNS <Badge intent="info">v2.0</Badge>

NICo answers DNS for everything it manages. Records are never authored by hand: they derive from the machine, BMC, and instance inventory in the `nico-api` database, appear when an interface or instance gains an address, and disappear when it loses one. This page covers the names NICo serves, how the site zone and per-segment subdomains are configured, and how reverse (PTR) resolution works. For the deployment side - the `nico-dns` service, the recursive resolver in front of it, and the fixed infrastructure service names - refer to [IP and Network Configuration](../provisioning/ip-and-network-configuration.md#3-dns-configuration).

## What Gets a Name

Every managed machine interface gets a hostname; [Host Naming](host-naming.md) covers how that label is chosen. These are the forward records NICo serves:

| Name | Answers for | Notes |
|---|---|---|
| `<hostname>.<domain>` | A host's primary interface and every BMC interface | The human-facing name, built by the host-naming strategy |
| `<machine-id>.adm.<domain>` | A host's primary interface | Keyed on the stable machine id |
| `<machine-id>.bmc.<domain>` | A machine's BMC interface | Host, predicted-host, and DPU machine ids |
| `<ip-hostname>.<segment subdomain>` | An overlay (DPU-managed) instance address | Always IP-derived (`10-1-2-3`, or the fully expanded IPv6 form), independent of the host-naming strategy |

Records serve as A or AAAA per address family, with a 300-second TTL by default. An interface that loses all of its addresses drops out of DNS until it has one again. Instances on `host_inband` segments publish no instance record: their address is the host's own interface address, which the machine records above already serve.

## The Site Zone and Segment Subdomains

The site's forward zone is seeded once, at first startup, from the `nico-api` config:

```toml
initial_domain_name = "mysite.example.com"
```

The domain is created only when no domain exists yet, and most sites use a single domain for their lifetime. Network segments then decide which of their interfaces and instances get names:

- Segments seeded from the config (`[networks.*]`) inherit the site domain automatically.
- Segments created later through the API or CLI name their subdomain explicitly: `nico-admin-cli network-segment create ... --subdomain-id <domain-id>` (required for `host-inband` segments; list domains with `nico-admin-cli domain show`).
- A segment with no subdomain produces no DNS at all: its interfaces and instances have no zone to live in.

## Reverse DNS

Reverse resolution mirrors the forward records that matter to humans:

- An address on a primary interface or a BMC interface answers PTR with `<hostname>.<domain>`.
- An overlay instance address answers PTR with its instance name.
- The `adm` and `bmc` machine-id forms are forward-only.

Reverse zones are derived, not managed. When a network segment is created, NICo derives the matching `in-addr.arpa` / `ip6.arpa` zone from each of its prefixes (for example, `192.0.2.0/24` becomes `2.0.192.in-addr.arpa`) and removes it again when the segment is deleted. Only octet-aligned IPv4 prefixes (/8, /16, /24, /32) and nibble-aligned IPv6 prefixes get a zone; anything else is skipped, since RFC 2317 classless delegation is out of scope. PTR answers themselves are matched by address, so alignment never blocks an answer; the derived zone rows define which reverse zones you delegate.

Like the forward zone, reverse zones must be delegated from your upstream DNS - or forwarded by your recursive resolver - to the `nico-dns` service address. NICo creates the zones but cannot delegate them for you.

## Server Behavior Worth Knowing

- `nico-dns` forwards A, AAAA, PTR, SOA, NS, CNAME, MX, and TXT queries to the API; any other type, and zone transfers, answer "not implemented". It never recurses. It publishes no NS or CNAME/MX/TXT records, so those types answer NOERROR with an empty answer section inside a held zone. SOA at a zone apex is answered. Put it behind your recursive resolver (a forward zone works best) rather than in a client's resolver list.
- Positive answers reflect the database live: there is no positive cache and no zone-serial machinery. A new or changed record is visible on the next query for it unless a negative answer for that name and type is cached as described in the next items. In that case, the record appears when the cache entry expires.
- Inside a zone NICo holds, a name that exists without the requested type (for example, only the other address family) answers NOERROR with an empty answer section, and a name that does not exist answers NXDomain; both carry the zone SOA and set the AA bit. A name outside every held zone answers Refused rather than NXDomain, since it may exist elsewhere.
- `nico-dns` caches the negative answers it gets from the API. A cached answer replays the same response code, AA bit, and SOA.
- NOERROR-with-no-data and NXDomain are cached for the shortest of the zone SOA record TTL, the SOA minimum, and `negative_cache_ttl_secs`, which defaults to 120 seconds. The SOA TTL in the first answer and in each replay is the time the cache entry has left.
- Refused carries no SOA and is cached for `negative_cache_ttl_secs`. ServFail from an upstream failure is cached for `negative_cache_servfail_ttl_secs`, which defaults to 5 seconds and is clamped to 1–300 seconds. This collapses a retry storm into one upstream call without outliving the recovery.
- The "not implemented" answer for an unsupported query type and the FORMERR for an unreadable query are decided before the API is asked and are not cached.
- Hosts learn their own FQDN over DHCP option 12 on every path; hosts served by a DPU also receive it as DHCP option 15.

## Resolvers Handed to Hosts

DHCP option 6 tells managed machines where to resolve, and it must point at the recursive resolver, never at `nico-dns` directly (`nico-dns` cannot answer external names or the infrastructure service names):

- On the site DHCP path, option 6 comes from the `nico-dhcp` Kea hook parameter `carbide-nameservers` (the `config.kea.hookParameters.nameservers` Helm value emits it).
- Hosts behind a DPU receive the DPU's own resolver set.
- The opt-in [DHCPv6 deployment](../provisioning/dhcpv6-deployment.md) advertises IPv6 recursive resolvers in option 23 through the `dhcp.v6DnsServers` Helm value.

## Unbound IPv6 Transport

The combined `nico-unbound` Service serves DNS on UDP/TCP 53 and, when `exporter.enabled` is true (chart default), exporter metrics on TCP 9167. `unbound.ipv6.enabled` is an optional Boolean with chart default `false`. An omitted or null map, or an omitted flag, also leaves the Service IPv4-only. `true` requests `PreferDualStack` while retaining IPv4 as the primary family. The flag controls this Service's exposure only. It does not change the image, startup command, mounted configuration, exporter, or the separate external DNS Services.

You supply the Unbound and exporter images. IPv6 enablement requires that Unbound consumes the configuration mounted at `/etc/unbound/local.conf.d`, listens on IPv6 UDP and TCP 53, retains its existing IPv4 listeners, and permits the intended IPv6 clients. The exporter must separately listen on IPv6 TCP 9167 when enabled. Both containers need the pod's working IPv6 network. Image tags alone do not establish these capabilities.

Use the image's existing configuration mechanism. For an image that includes the chart's fragments and has no listener directives elsewhere, the following umbrella-chart values illustrate the complete DNS listener and access configuration. Replace the IPv6 prefix with the source network Unbound will actually see. Preserve your site's existing IPv4 listener addresses and access rules. The example repeats the chart's default IPv4 access rule. Omit listener directives already present elsewhere in the image's effective configuration, since explicit interfaces accumulate. A subchart installation uses these values without the outer `unbound:` key.

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

For Kustomize, configure the image through the existing `deploy/nico-unbound-base/local.conf.d/access_control.conf` and change only `nico-unbound`'s Service `ipFamilyPolicy` to `PreferDualStack` in the site overlay, retaining its IPv4 primary family and all three ports. The base remains IPv4-only. The root deployment publishes this combined Service through a LoadBalancer, so IPv6 enablement also requires compatible virtual IP address (VIP) allocation and routing there. The Helm chart's separate external DNS Services remain IPv4-only. An internal dual-stack Service does not provide an external IPv6 resolver VIP for bare-metal hosts.

Before advertising an IPv6 resolver to clients, validate the deployed images from an intended client network:

- Query a configured local name and a name resolved through the existing forwarders over IPv6 UDP and TCP 53. Verify the expected DNS answers. Check the pod first, then the Service's IPv6 address after enabling exposure.
- Repeat representative DNS queries over IPv4 and confirm the existing IPv4 Service address is retained.
- Fetch the exporter's metrics through IPv6 and the existing `nico-unbound:9167` IPv4 endpoint when the exporter is enabled.
- Confirm an IPv4-only installation still upgrades with its existing values. The default-off flag introduces no listener configuration or pod-template change.

DNS transport and record type are independent: either IPv4 or IPv6 transport can carry A or AAAA queries. A TCP readiness connection proves neither DNS answers nor IPv6 Service routing. External IPv6 DNS additionally requires a reachable resolver VIP and advertising it to clients through the site's DHCPv6 configuration.

## Troubleshooting

The following table lists common situations and where to look:

| Symptom | Likely cause / action |
|---|---|
| A machine interface has no name | Only primary and BMC interfaces publish records; the interface can also be addressless, or its segment can lack a subdomain. `nico-admin-cli managed-host show <machine-id>` shows interfaces and the primary flag; the `/admin/ipam/dns` web page lists every record NICo is serving. |
| An instance has no name | The segment's `--subdomain-id` is unset, the address predates instance-hostname population (records populate for addresses allocated going forward), or the instance is on a `host_inband` segment (the host's machine record serves that address). |
| PTR is missing while the forward name resolves | Only the `<hostname>.<domain>` and instance forms answer PTR; the `adm` and `bmc` machine-id forms are forward-only. |
| Hosts cannot resolve external names | DHCP option 6 points at `nico-dns` (authoritative only) instead of the recursive resolver. Refer to [IP and Network Configuration](../provisioning/ip-and-network-configuration.md#32-unbound-recursive-resolver-for-managed-machines). |
