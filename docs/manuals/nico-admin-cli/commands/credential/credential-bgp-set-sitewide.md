# `nico-admin-cli credential bgp set-sitewide`

*[Hardware commands](../../hardware.md) › [credential](./credential.md) › [bgp](./credential-bgp.md) › **set-sitewide***

## NAME

nico-admin-cli-credential-bgp-set-sitewide - Set the site-wide leaf BGP
password

## SYNOPSIS

**nico-admin-cli credential bgp set-sitewide** \<**--password**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Set the site-wide leaf BGP password

## OPTIONS

**--password** *\<PASSWORD\>*  
Leaf BGP session password

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
nico-admin-cli credential bgp set-sitewide --password mynewpassword
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
