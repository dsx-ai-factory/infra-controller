# `nico-admin-cli host generate-host-uefi-password`

*[Hardware commands](../../hardware.md) › [host](./host.md) › **generate-host-uefi-password***

## NAME

nico-admin-cli-host-generate-host-uefi-password - Generates a string
that can be a site-default host UEFI password in Vault

## SYNOPSIS

**nico-admin-cli host generate-host-uefi-password** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Generates a string that can be a site-default host UEFI password in
Vault

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
nico-admin-cli host generate-host-uefi-password
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
