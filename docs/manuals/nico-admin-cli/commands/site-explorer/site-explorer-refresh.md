# `nico-admin-cli site-explorer refresh`

_[Tenant commands](../../tenant.md) › [site-explorer](./site-explorer.md) › **refresh**_

## NAME

nico-admin-cli-site-explorer-refresh - Immediately probes a BMC endpoint
and persists the report.

## SYNOPSIS

**nico-admin-cli site-explorer refresh** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*ADDRESS*\>

## DESCRIPTION

Immediately probes a BMC endpoint and persists the report.

## OPTIONS

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
BMC IP address

## Examples

```sh
nico-admin-cli site-explorer refresh 192.0.2.10
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
