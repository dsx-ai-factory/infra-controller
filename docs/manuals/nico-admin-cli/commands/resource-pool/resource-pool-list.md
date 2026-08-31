# `nico-admin-cli resource-pool list`

*[Network commands](../../network.md) › [resource-pool](./resource-pool.md) › **list***

## NAME

nico-admin-cli-resource-pool-list - List all resource pools with stats

## SYNOPSIS

**nico-admin-cli resource-pool list** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

List all resource pools with stats

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
nico-admin-cli resource-pool list
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
