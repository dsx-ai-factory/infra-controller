# `nico-admin-cli route-server get`

*[Network commands](../../network.md) › [route-server](./route-server.md) › **get***

## NAME

nico-admin-cli-route-server-get - Get all route servers

## SYNOPSIS

**nico-admin-cli route-server get** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Get all route servers

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

## Examples

```sh
nico-admin-cli route-server get
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
