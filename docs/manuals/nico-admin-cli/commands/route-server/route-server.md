# `nico-admin-cli route-server`

*[Network commands](../../network.md) › **route-server***

## NAME

nico-admin-cli-route-server - Route server handling

## SYNOPSIS

**nico-admin-cli route-server** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Route server handling

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
| [`get`](./route-server-get.md) | Get all route servers |
| [`add`](./route-server-add.md) | Add route server addresses |
| [`remove`](./route-server-remove.md) | Remove route server addresses |
| [`replace`](./route-server-replace.md) | Replace all route server addresses |

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
