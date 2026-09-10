# `nico-admin-cli site-explorer remediation`

_[Tenant commands](../../tenant.md) › [site-explorer](./site-explorer.md) › **remediation**_

## NAME

nico-admin-cli-site-explorer-remediation - Control remediation actions
for an explored endpoint.

## SYNOPSIS

**nico-admin-cli site-explorer remediation** \[**--pause**\]
\[**--resume**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*ADDRESS*\>

## DESCRIPTION

Control remediation actions for an explored endpoint.

## OPTIONS

**--pause**  
Pause remediation actions

**--resume**  
Resume remediation actions

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
BMC IP address of the endpoint

## Examples

```sh
nico-admin-cli site-explorer remediation 192.0.2.10 --pause
nico-admin-cli site-explorer remediation 192.0.2.10 --resume
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
