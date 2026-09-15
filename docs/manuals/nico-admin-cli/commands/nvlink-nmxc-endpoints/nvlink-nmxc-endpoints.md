# `nico-admin-cli nvlink-nmxc-endpoints`

_[Hardware commands](../../hardware.md) › **nvlink-nmxc-endpoints**_

## NAME

nico-admin-cli-nvlink-nmxc-endpoints - Rack chassis serial → NMX-C
endpoint mappings

## SYNOPSIS

**nico-admin-cli nvlink-nmxc-endpoints** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Rack chassis serial → NMX-C endpoint mappings

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
| [`show`](./nvlink-nmxc-endpoints-show.md) | List chassis serial → NMX-C endpoint mappings (optionally one serial) |
| [`create`](./nvlink-nmxc-endpoints-create.md) | Insert a mapping for a chassis serial |
| [`update`](./nvlink-nmxc-endpoints-update.md) | Change the endpoint URL for a chassis serial |
| [`delete`](./nvlink-nmxc-endpoints-delete.md) | Remove a mapping by chassis serial |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
