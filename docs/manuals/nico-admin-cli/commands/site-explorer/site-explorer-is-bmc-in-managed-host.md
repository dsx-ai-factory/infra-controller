# `nico-admin-cli site-explorer is-bmc-in-managed-host`

*[Tenant commands](../../tenant.md) › [site-explorer](./site-explorer.md) › **is-bmc-in-managed-host***

## NAME

nico-admin-cli-site-explorer-is-bmc-in-managed-host

## SYNOPSIS

**nico-admin-cli site-explorer is-bmc-in-managed-host** \[**--mac**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*ADDRESS*\>

## DESCRIPTION

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
nico-admin-cli site-explorer is-bmc-in-managed-host 192.0.2.10
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
