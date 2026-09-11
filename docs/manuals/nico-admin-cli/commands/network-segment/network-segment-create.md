# `nico-admin-cli network-segment create`

*[Network commands](../../network.md) › [network-segment](./network-segment.md) › **create***

## NAME

nico-admin-cli-network-segment-create - Create Network Segment

## SYNOPSIS

**nico-admin-cli network-segment create** \<**--name**\> \[**--id**\]
\[**--vpc-id**\] \[**--subdomain-id**\] \[**--mtu**\] \<**--prefix**\>
\[**--gateway**\] \[**--reserve-first**\] \[**--segment-type**\]
\[**--infer-slaac-eui64-addresses**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Create Network Segment

## OPTIONS

**--name** *\<NAME\>*  
Network segment name

**--id** *\<NetworkSegmentId\>*  
Optional network segment ID to use instead of allowing the API server to
generate one

**--vpc-id** *\<VpcId\>*  
Optional VPC ID to attach the new segment to

**--subdomain-id** *\<DomainId\>*  
DNS subdomain ID used for DHCP and DNS records on the segment. Required
for segments of type host-inband

**--mtu** *\<MTU\>*  
Optional MTU for the segment. Defaults to 9000 for tenant segments and
1500 for other segment types

**--prefix** *\<CIDR-prefix\>*  
Network prefix in CIDR notation. Repeat once per address family

**--gateway** *\<IPv4-address\>*  
IPv4 gateway for the IPv4 prefix

**--reserve-first** *\<COUNT\>* \[default: 0\]  
Number of addresses to reserve before dynamic allocation starts

**--segment-type** *\<SEGMENT_TYPE\>* \[default: tenant\]  
Network segment type\

\
*Possible values:*

- tenant

- admin

- underlay

- host-inband

**--infer-slaac-eui64-addresses**  
Infer modified EUI-64 SLAAC addresses for stateless DHCPv6 clients and
add them to interface address state. Off by default; use only when
clients derive SLAAC addresses from their MAC addresses, and only on a
dynamic segment with exactly one IPv6 /64. Existing IPv6 addresses are
not replaced

**--extended**  
Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

**--sort-by** *\<SORT_BY\>* \[default: primary-id\]  
Sort output by specified field\

\
*Possible values:*

- primary-id: Sort by the primary ID

- state: Sort by state

**-h**, **--help**  
Print help (see a summary with -h)

## Examples

```sh
nico-admin-cli --cloud-unsafe-op=admin network-segment create --name tenant-segment-1 --vpc-id 12345678-1234-5678-90ab-cdef01234567 --prefix 10.0.0.0/24 --gateway 10.0.0.1
nico-admin-cli --cloud-unsafe-op=admin network-segment create --name host-inband-a --segment-type host-inband --id 12345678-1234-5678-90ab-cdef01234567 --prefix 192.0.2.0/24 --gateway 192.0.2.1 --prefix 2001:db8::/64 --subdomain-id abcdef01-2345-6789-abcd-ef0123456789 --infer-slaac-eui64-addresses
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
