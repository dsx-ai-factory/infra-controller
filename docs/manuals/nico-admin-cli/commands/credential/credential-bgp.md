# `nico-admin-cli credential bgp`

_[Hardware commands](../../hardware.md) › [credential](./credential.md) › **bgp**_

## NAME

nico-admin-cli-credential-bgp - Manage leaf BGP passwords

## SYNOPSIS

**nico-admin-cli credential bgp** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Manage leaf BGP passwords

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

## Subcommands

| Subcommand | Description |
|---|---|
| [`set-sitewide`](./credential-bgp-set-sitewide.md) | Set the site-wide leaf BGP password |
| [`delete-sitewide`](./credential-bgp-delete-sitewide.md) | Delete the site-wide leaf BGP password |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
