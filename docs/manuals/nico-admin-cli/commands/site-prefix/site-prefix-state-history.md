# `nico-admin-cli site-prefix state-history`

*[Network commands](../../network.md) › [site-prefix](./site-prefix.md) › **state-history***

## NAME

nico-admin-cli-site-prefix-state-history - Show all lifecycle state
history for a SitePrefix, or an empty collection

## SYNOPSIS

**nico-admin-cli site-prefix state-history** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*SITE_PREFIX_ID*\>

## DESCRIPTION

Show all lifecycle state history for a SitePrefix, or an empty
collection

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

\<*SITE_PREFIX_ID*\>  
SitePrefix history to show

## Examples

```sh
nico-admin-cli site-prefix state-history 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
