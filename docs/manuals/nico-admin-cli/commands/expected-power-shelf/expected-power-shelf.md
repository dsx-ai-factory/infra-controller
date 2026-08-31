# `nico-admin-cli expected-power-shelf`

*[Tenant commands](../../tenant.md) › **expected-power-shelf***

## NAME

nico-admin-cli-expected-power-shelf - Expected power shelf handling

## SYNOPSIS

**nico-admin-cli expected-power-shelf** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Expected power shelf handling

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
| [`show`](./expected-power-shelf-show.md) | Show expected power shelf |
| [`add`](./expected-power-shelf-add.md) | Add expected power shelf |
| [`delete`](./expected-power-shelf-delete.md) | Delete expected power shelf |
| [`update`](./expected-power-shelf-update.md) | Update expected power shelf |
| [`replace-all`](./expected-power-shelf-replace-all.md) | Replace all expected power shelves |
| [`erase`](./expected-power-shelf-erase.md) | Erase all expected power shelves |

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
