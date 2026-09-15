# `nico-admin-cli sku`

_[Hardware commands](../../hardware.md) › **sku**_

## NAME

nico-admin-cli-sku - Manage machine SKUs

## SYNOPSIS

**nico-admin-cli sku** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Manage machine SKUs

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
| [`show`](./sku-show.md) | Show SKU information |
| [`show-machines`](./sku-show-machines.md) | Show what machines are assigned a SKU |
| [`generate`](./sku-generate.md) | Generate SKU information from an existing machine |
| [`create`](./sku-create.md) | Create SKUs from a file |
| [`delete`](./sku-delete.md) | Delete a SKU |
| [`assign`](./sku-assign.md) | Assign a SKU to a machine |
| [`unassign`](./sku-unassign.md) | Unassign a SKU from a machine |
| [`verify`](./sku-verify.md) | Verify a machine against its SKU |
| [`update-metadata`](./sku-update-metadata.md) | Update the metadata of a SKU |
| [`bulk-update-metadata`](./sku-bulk-update-metadata.md) | Update multiple SKU's metadata from a file |
| [`replace`](./sku-replace.md) | Replace the component list of a SKU |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
