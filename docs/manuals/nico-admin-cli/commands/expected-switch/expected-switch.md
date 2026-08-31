# `nico-admin-cli expected-switch`

*[Tenant commands](../../tenant.md) › **expected-switch***

## NAME

nico-admin-cli-expected-switch - Expected switch handling

## SYNOPSIS

**nico-admin-cli expected-switch** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Expected switch handling

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
| [`show`](./expected-switch-show.md) | Show expected switch |
| [`add`](./expected-switch-add.md) | Add expected switch |
| [`delete`](./expected-switch-delete.md) | Delete expected switch |
| [`update`](./expected-switch-update.md) | Update expected switch |
| [`replace-all`](./expected-switch-replace-all.md) | Replace all expected switches |
| [`erase`](./expected-switch-erase.md) | Erase all expected switches |

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
