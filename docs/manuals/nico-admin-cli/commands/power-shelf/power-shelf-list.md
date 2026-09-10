# `nico-admin-cli power-shelf list`

_[Hardware commands](../../hardware.md) › [power-shelf](./power-shelf.md) › **list**_

## NAME

nico-admin-cli-power-shelf-list - List all power shelves

## SYNOPSIS

**nico-admin-cli power-shelf list** \[**--deleted**\]
\[**--controller-state**\] \[**--bmc-mac**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

List all power shelves

## OPTIONS

**--deleted** *\<DELETED\>* \[default: exclude\]  
Include deleted power shelves\

\
*Possible values:*

- exclude: Exclude deleted resources (default behavior)

- only: Return only deleted resources

- include: Include both deleted and non-deleted resources

**--controller-state** *\<CONTROLLER_STATE\>*  
Filter by controller state (e.g. "ready", "initializing", "error")

**--bmc-mac** *\<BMC_MAC\>*  
Filter by BMC MAC address

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

## Examples

```sh
nico-admin-cli power-shelf list
nico-admin-cli power-shelf list --deleted include
nico-admin-cli power-shelf list --controller-state ready
nico-admin-cli power-shelf list --bmc-mac 00:11:22:33:44:55
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
