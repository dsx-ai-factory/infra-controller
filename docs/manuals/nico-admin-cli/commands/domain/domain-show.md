# `nico-admin-cli domain show`

*[Network commands](../../network.md) › [domain](./domain.md) › **show***

## NAME

nico-admin-cli-domain-show - Display Domain information

## SYNOPSIS

**nico-admin-cli domain show** \[**-a**\|**--all**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \[*DOMAIN*\]

## DESCRIPTION

Display Domain information

## OPTIONS

**-a**, **--all**  
Show all domains (DEPRECATED)

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

\[*DOMAIN*\]  
The domain to query, leave empty for all (default)

## Examples

```sh
nico-admin-cli domain show
nico-admin-cli domain show 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
