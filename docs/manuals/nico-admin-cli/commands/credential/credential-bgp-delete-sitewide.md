# `nico-admin-cli credential bgp delete-sitewide`

*[Hardware commands](../../hardware.md) › [credential](./credential.md) › [bgp](./credential-bgp.md) › **delete-sitewide***

## NAME

nico-admin-cli-credential-bgp-delete-sitewide - Delete the site-wide
leaf BGP password

## SYNOPSIS

**nico-admin-cli credential bgp delete-sitewide** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Delete the site-wide leaf BGP password

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

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
