# `nico-admin-cli ipxe-template`

*[Tenant commands](../../tenant.md) › **ipxe-template***

## NAME

nico-admin-cli-ipxe-template - iPXE template management

## SYNOPSIS

**nico-admin-cli ipxe-template** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

iPXE template management

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
| [`show`](./ipxe-template-show.md) | Show iPXE templates (all, or one by ID). |

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
