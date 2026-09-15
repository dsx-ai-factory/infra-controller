# `nico-admin-cli ipxe-template show`

_[Tenant commands](../../tenant.md) › [ipxe-template](./ipxe-template.md) › **show**_

## NAME

nico-admin-cli-ipxe-template-show - Show iPXE templates (all, or one by
name).

## SYNOPSIS

**nico-admin-cli ipxe-template show** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \[*ID*\]

## DESCRIPTION

Show iPXE templates (all, or one by name).

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

\[*ID*\]  
Template ID (UUID); omit to list all.

## Examples

```sh
nico-admin-cli ipxe-template show
nico-admin-cli ipxe-template show 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
