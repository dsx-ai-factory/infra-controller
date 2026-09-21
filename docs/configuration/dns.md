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

For an address with unambiguous ownership and a publishable name, PTR targets are:

- An address on a primary interface or a BMC interface answers PTR with `<hostname>.<domain>`.
- An overlay instance address answers PTR with its IP-derived instance FQDN.
- The `adm` and `bmc` machine-id forms are forward-only.

PTR answers are derived from address and hostname inventory, independently of stored reverse zones. NICo publishes a PTR only when the address has exactly one inventory owner and one publishable target. Ownership is checked across machine and overlay instance addresses, not per VPC; an owner without a publishable name still counts toward ambiguity. No `in-addr.arpa` / `ip6.arpa` zone is served. A published PTR sets the AA bit without claiming authority over its enclosing reverse zone. Prefix alignment does not affect PTR eligibility, and an address need not belong to a managed prefix.

For supported query types, the following reverse queries return `REFUSED`, with the AA bit clear and no SOA:

- an address NICo does not publish a PTR for,
- an address whose owner is ambiguous,
- incomplete reverse names or labels that do not identify an address,
- A, AAAA, SOA, NS, CNAME, MX, or TXT queries under `in-addr.arpa` / `ip6.arpa`.

Unsupported types, including ANY, SRV, AXFR, and IXFR, return `NOTIMP` before reverse lookup is attempted.

The REFUSED responses do not assert that the name is absent: NICo is not authoritative for the reverse tree, and another server may publish it. There is no reverse SOA or authoritative negative answer, so a resolver cannot cache "this address has no PTR" on NICo's authority. The `nico-dns` negative cache stores REFUSED responses for `negative_cache_ttl_secs`.

Reverse names cannot be managed as domains: the domain API rejects a `CreateDomain` or `UpdateDomain` name at or below `in-addr.arpa` or `ip6.arpa` with `INVALID_ARGUMENT`, ignoring case, surrounding whitespace, and trailing dots.

## Server Behavior Worth Knowing

- `nico-dns` supports A, AAAA, PTR, SOA, NS, CNAME, MX, and TXT queries; unsupported types and zone transfers return `NOTIMP`. It never recurses. It publishes no NS, CNAME, MX, or TXT records: in a held forward zone, those types return NOERROR with an empty answer section (NODATA) at existing names, or NXDOMAIN at missing names. SOA is answered at a forward zone apex, but not at reverse names. Put it behind your recursive resolver rather than in a client's resolver list.
- Positive answers reflect the database live: there is no positive cache and no zone-serial machinery. A new or changed record is visible on the next query for it unless a negative answer for that name and type is cached as described in the next items. In that case, the record appears when the cache entry expires.
- For supported types inside a held forward zone, an existing name without the requested type answers NODATA, and a missing name answers NXDOMAIN; both carry the zone SOA and set the AA bit. The zone apex and names with published descendants count as existing names. A forward name outside every held zone returns REFUSED rather than NXDOMAIN. Reverse queries follow the separate rules above.
- `nico-dns` caches the negative answers it gets from the API. A cached answer replays the same response code, AA bit, and SOA.
- NOERROR-with-no-data and NXDomain are cached for the shortest of the zone SOA record TTL, the SOA minimum, and `negative_cache_ttl_secs`, which defaults to 120 seconds. The SOA TTL in the first answer and in each replay is the time the cache entry has left.
- Refused carries no SOA and is cached for `negative_cache_ttl_secs`. ServFail from an upstream failure is cached for `negative_cache_servfail_ttl_secs`, which defaults to 5 seconds and is clamped to 1–300 seconds. Cached failures reduce repeated API calls but can remain visible until expiry after the API recovers; concurrent cache misses are not coalesced.
- The "not implemented" answer for an unsupported query type and the FORMERR for an unreadable query are decided before the API is asked and are not cached.
- On DHCPv4 paths, hosts learn their own FQDN over option 12; hosts served by a DPU also receive it as option 15.

## Resolvers Handed to Hosts

DHCP option 6 tells managed machines where to resolve, and it must point at the recursive resolver, never at `nico-dns` directly (`nico-dns` cannot answer external names or the infrastructure service names):

- On the site DHCP path, option 6 comes from the `nico-dhcp` Kea hook parameter `carbide-nameservers` (the `config.kea.hookParameters.nameservers` Helm value emits it).
- Hosts behind a DPU receive the DPU's own resolver set.
- DHCPv6 option 23 advertises configured IPv6 resolvers. The site DHCPv6 server is opt-in; refer to the [DHCP configuration](../provisioning/ip-and-network-configuration.md) for deployment settings.

## Troubleshooting

The following table lists common situations and where to look:

| Symptom | Likely cause / action |
|---|---|
| A machine interface has no name | Only primary and BMC interfaces publish records; the interface can also be addressless, or its segment can lack a subdomain. `nico-admin-cli managed-host show <machine-id>` shows interfaces and the primary flag; the `/admin/ipam/dns` web page lists every record NICo is serving. |
| An instance has no name | The segment's `--subdomain-id` is unset, the address predates instance-hostname population (records populate for addresses allocated going forward), or the instance is on a `host_inband` segment (the host's machine record serves that address). |
| PTR returns a different name than the forward query | Several forward names can resolve to the same address. PTR selects `<hostname>.<domain>` or the instance's IP-derived FQDN, not the `adm` or `bmc` machine-id forms. |
| PTR returns `REFUSED` while a forward name resolves | Check for multiple inventory owners of the address, including across VPCs, and for a cached REFUSED answer. Forward publication does not guarantee unambiguous PTR ownership. |
| Another reverse query returns `REFUSED` | Expected for supported non-PTR types and reverse names without a publishable, unambiguous PTR. NICo serves no reverse zone. Unsupported types return `NOTIMP` instead. |
| Hosts cannot resolve external names | DHCP option 6 points at `nico-dns` (authoritative only) instead of the recursive resolver. Refer to [IP and Network Configuration](../provisioning/ip-and-network-configuration.md#32-unbound-recursive-resolver-for-managed-machines). |
