# `nico-admin-cli site-explorer get-report managed-host`

_[Tenant commands](../../tenant.md) › [site-explorer](./site-explorer.md) › [get-report](./site-explorer-get-report.md) › **managed-host**_

## NAME

nico-admin-cli-site-explorer-get-report-managed-host - Get discovered
host details.

## SYNOPSIS

**nico-admin-cli site-explorer get-report managed-host**
\[**-v**\|**--vendor**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \[*ADDRESS*\]

## DESCRIPTION

Get discovered host details.

## OPTIONS

**-v**, **--vendor** *\<VENDOR\>*  
Filter based on vendor. Valid only for table view.

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

\[*ADDRESS*\]  
BMC IP address of host or DPU

## Examples

```sh
nico-admin-cli site-explorer get-report managed-host
nico-admin-cli site-explorer get-report managed-host 192.0.2.10
nico-admin-cli site-explorer get-report managed-host --vendor nvidia
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
