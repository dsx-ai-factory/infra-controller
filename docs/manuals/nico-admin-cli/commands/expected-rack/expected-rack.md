# `nico-admin-cli expected-rack`

_[Tenant commands](../../tenant.md) › **expected-rack**_

## NAME

nico-admin-cli-expected-rack - Expected rack handling

## SYNOPSIS

**nico-admin-cli expected-rack** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Expected rack handling

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
| [`show`](./expected-rack-show.md) | Show expected rack |
| [`add`](./expected-rack-add.md) | Add expected rack |
| [`delete`](./expected-rack-delete.md) | Delete expected rack |
| [`update`](./expected-rack-update.md) | Update expected rack |
| [`replace-all`](./expected-rack-replace-all.md) | Replace all expected racks |
| [`erase`](./expected-rack-erase.md) | Erase all expected racks |

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
