# `nico-admin-cli network-segment add-prefix`

*[Network commands](../../network.md) › [network-segment](./network-segment.md) › **add-prefix***

## NAME

nico-admin-cli-network-segment-add-prefix - Append the missing address
family to an existing Underlay segment

## SYNOPSIS

```text
nico-admin-cli network-segment add-prefix <--id>
<--prefix> [--gateway] [--reserve-first]
[--dhcpv6-link-address] [--extended] [--sort-by]
[-h|--help]
```

## DESCRIPTION

Append a prefix to a live, Ready Underlay segment with exactly one
prefix and no VPC attachment. Existing prefix IDs, addresses, policy and
lifecycle remain unchanged, as does the stored initial TOML definition.
Matching any stored same-family prefix and settings returns the segment
without a write; different same-family settings fail. Overlap with any
segment prefix or VPC-scoped VPC prefix is rejected; a global VPC parent
may overlap an unattached underlay. Reverse DNS is committed with the
addition for zone-aligned prefixes. Existing SLAAC inference policy
applies to the new prefix. Site routing, router advertisements and DHCP
relays are not configured. Requires administrator credentials and the
global --cloud-unsafe-op acknowledgement. Do not use --cloud-unsafe-op
against a production site. Output supports ascii-table, json and yaml;
csv is not accepted.

## OPTIONS

`--id <NetworkSegmentId>`

ID of the existing Underlay network segment

`--prefix <CIDR-prefix>`

Network CIDR with an explicit numeric prefix length and no host bits;
IPv4 must be shorter than /32. Adds the segment's missing family or
matches existing settings without a write

`--gateway <IPv4-address>`

Required for IPv4: an IPv4 gateway within the added prefix. Not accepted
for IPv6

`--reserve-first <COUNT> [default: 0]`

Non-negative number of initial addresses excluded from dynamic
allocation in the new prefix

`--dhcpv6-link-address <IPv6-address>`

Optional IPv6 DHCP relay link-address selector for an IPv6 prefix; it
can be outside the prefix. Omit for prefix-based selection. Exact
matches take precedence: choose a selector that does not redirect
another network's requests

`--extended`

Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

`--sort-by <SORT_BY> [default: primary-id]`

Sort output by specified field

*Possible values:*

> - primary-id: Sort by the primary ID
>
> - state: Sort by state

`-h, --help`

Print help (see a summary with '-h')

## Examples

```sh
nico-admin-cli --cloud-unsafe-op=admin network-segment add-prefix --id 12345678-1234-5678-90ab-cdef01234567 --prefix 192.0.2.0/24 --gateway 192.0.2.1
nico-admin-cli --cloud-unsafe-op=admin network-segment add-prefix --id 12345678-1234-5678-90ab-cdef01234567 --prefix 2001:db8::/64 --dhcpv6-link-address 2001:db8:ffff::1 --reserve-first 5
```

---

**Related:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
