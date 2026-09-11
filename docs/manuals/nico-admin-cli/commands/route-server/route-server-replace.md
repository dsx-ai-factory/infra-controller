# `nico-admin-cli route-server replace`

_[Network commands](../../network.md) › [route-server](./route-server.md) › **replace**_

## NAME

nico-admin-cli-route-server-replace - Replace all route server addresses

## SYNOPSIS

**nico-admin-cli route-server replace** \[**--source-type**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\] \[*IP*\]

## DESCRIPTION

Replace all route server addresses

## OPTIONS

**--source-type** *\<SOURCE_TYPE\>* \[default: admin_api\]  
The source_type to use for the target addresses. Defaults to admin_api.\

\
*Possible values:*

- admin_api

- config_file

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

\[*IP*\]  
Comma-separated list of IP addresses

## Examples

```sh
nico-admin-cli route-server replace 10.0.0.1,10.0.0.2,10.0.0.3
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
