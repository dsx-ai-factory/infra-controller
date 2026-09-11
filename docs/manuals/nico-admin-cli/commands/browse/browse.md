# `nico-admin-cli browse`

*[Hardware commands](../../hardware.md) › **browse***

## NAME

nico-admin-cli-browse - Browse subsystem resource trees via the API
server

## SYNOPSIS

**nico-admin-cli browse** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Browse subsystem resource trees via the API server

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
| [`redfish`](./browse-redfish.md) | Browse a Redfish resource tree via the API server |
| [`ufm`](./browse-ufm.md) | Browse a UFM fabric via the API server |
| [`nmxc`](./browse-nmxc.md) | Run an NMX-C browse operation via the API server |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
