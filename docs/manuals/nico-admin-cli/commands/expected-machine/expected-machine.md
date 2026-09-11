# `nico-admin-cli expected-machine`

_[Tenant commands](../../tenant.md) › **expected-machine**_

## NAME

nico-admin-cli-expected-machine - Expected machine handling

## SYNOPSIS

**nico-admin-cli expected-machine** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Expected machine handling

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
| [`show`](./expected-machine-show.md) | Show expected machine data |
| [`add`](./expected-machine-add.md) | Add expected machine |
| [`delete`](./expected-machine-delete.md) | Delete expected machine |
| [`patch`](./expected-machine-patch.md) | Patch expected machine (partial update, preserves unprovided fields). |
| [`update`](./expected-machine-update.md) | Update expected machine from JSON file (full replacement, consistent with API). |
| [`replace-all`](./expected-machine-replace-all.md) | Replace all entries in the expected machines table with the entries from an inputted json file. |
| [`erase`](./expected-machine-erase.md) | Erase all expected machines |

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
