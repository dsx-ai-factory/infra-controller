# `nico-admin-cli site-explorer explore`

*[Tenant commands](../../tenant.md) › [site-explorer](./site-explorer.md) › **explore***

## NAME

nico-admin-cli-site-explorer-explore - Asks carbide-api to explore a
single host and prints the report. Does not store it.

## SYNOPSIS

**nico-admin-cli site-explorer explore** \[**--mac**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*ADDRESS*\>

## DESCRIPTION

Asks carbide-api to explore a single host and prints the report. Does
not store it.

## OPTIONS

**--mac** *\<MAC\>*  
The MAC address the BMC sent DHCP from

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

\<*ADDRESS*\>  
BMC IP address or hostname with optional port

## Examples

```sh
nico-admin-cli site-explorer explore 192.0.2.10
nico-admin-cli site-explorer explore 192.0.2.10 --mac 00:11:22:33:44:55
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
