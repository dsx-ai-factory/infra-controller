# `nico-admin-cli resource-pool grow`

*[Network commands](../../network.md) › [resource-pool](./resource-pool.md) › **grow***

## NAME

nico-admin-cli-resource-pool-grow - Add capacity to one or more resource
pools from a TOML file. See carbide-api admin_grow_resource_pool docs
for example TOML.

## SYNOPSIS

**nico-admin-cli resource-pool grow** \<**-f**\|**--filename**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Add capacity to one or more resource pools from a TOML file. See
carbide-api admin_grow_resource_pool docs for example TOML.

## OPTIONS

**-f**, **--filename** *\<FILENAME\>*  
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
nico-admin-cli resource-pool grow --filename ./grow-pools.toml
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
