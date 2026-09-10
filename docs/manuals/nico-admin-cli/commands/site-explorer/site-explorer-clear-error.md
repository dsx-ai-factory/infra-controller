# `nico-admin-cli site-explorer clear-error`

_[Tenant commands](../../tenant.md) › [site-explorer](./site-explorer.md) › **clear-error**_

## NAME

nico-admin-cli-site-explorer-clear-error - Clear the last known error
for the BMC in the latest site exploration report.

## SYNOPSIS

**nico-admin-cli site-explorer clear-error** \[**--mac**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*ADDRESS*\>

## DESCRIPTION

Clear the last known error for the BMC in the latest site exploration
report.

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
nico-admin-cli site-explorer clear-error 192.0.2.10
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
