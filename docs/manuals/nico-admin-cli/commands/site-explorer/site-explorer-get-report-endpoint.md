# `nico-admin-cli site-explorer get-report endpoint`

*[Tenant commands](../../tenant.md) › [site-explorer](./site-explorer.md) › [get-report](./site-explorer-get-report.md) › **endpoint***

## NAME

nico-admin-cli-site-explorer-get-report-endpoint - Get Endpoint details.

## SYNOPSIS

**nico-admin-cli site-explorer get-report endpoint**
\[**-v**\|**--vendor**\] \[**--unpairedonly**\] \[**--erroronly**\]
\[**--successonly**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \[*ADDRESS*\]

## DESCRIPTION

Get Endpoint details.

## OPTIONS

**-v**, **--vendor** *\<VENDOR\>*  
Filter based on vendor. Valid only for table view.

**--unpairedonly**  
By default shows all endpoints. If wants to see unpairedonly, choose
this option.

**--erroronly**  
Show only endpoints which have error.

**--successonly**  
Show only endpoints which have no error.

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
BMC IP address of Endpoint.

## Examples

```sh
nico-admin-cli site-explorer get-report endpoint
nico-admin-cli site-explorer get-report endpoint 192.0.2.10
nico-admin-cli site-explorer get-report endpoint --erroronly --vendor nvidia
nico-admin-cli site-explorer get-report endpoint --unpairedonly
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
