# `nico-admin-cli switch list`

_[Hardware commands](../../hardware.md) › [switch](./switch.md) › **list**_

## NAME

nico-admin-cli-switch-list - List all switches

## SYNOPSIS

**nico-admin-cli switch list** \[**--deleted**\]
\[**--controller-state**\] \[**--bmc-mac**\] \[**--nvos-mac**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

List all switches

## OPTIONS

**--deleted** *\<DELETED\>* \[default: exclude\]  
Include deleted switches\

\
*Possible values:*

- exclude: Exclude deleted resources (default behavior)

- only: Return only deleted resources

- include: Include both deleted and non-deleted resources

**--controller-state** *\<CONTROLLER_STATE\>*  
Filter by controller state (e.g. "ready", "initializing", "error")

**--bmc-mac** *\<BMC_MAC\>*  
Filter by BMC MAC address

**--nvos-mac** *\<NVOS_MAC\>*  
Filter by NVOS MAC address

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
nico-admin-cli switch list
nico-admin-cli switch list --deleted include
nico-admin-cli switch list --controller-state ready
nico-admin-cli switch list --bmc-mac 00:11:22:33:44:55
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
