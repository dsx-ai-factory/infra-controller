# `nico-admin-cli route-server remove`

*[Network commands](../../network.md) › [route-server](./route-server.md) › **remove***

## NAME

nico-admin-cli-route-server-remove - Remove route server addresses

## SYNOPSIS

**nico-admin-cli route-server remove** \[**--source-type**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\] \[*IP*\]

## DESCRIPTION

Remove route server addresses

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
nico-admin-cli route-server remove 10.0.0.1,10.0.0.2
nico-admin-cli route-server remove 10.0.0.1 --source-type config_file
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
