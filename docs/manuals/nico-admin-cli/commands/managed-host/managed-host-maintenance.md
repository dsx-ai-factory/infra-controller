# `nico-admin-cli managed-host maintenance`

*[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › **maintenance***

## NAME

nico-admin-cli-managed-host-maintenance - Switch a machine in/out of
maintenance mode

## SYNOPSIS

**nico-admin-cli managed-host maintenance** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Switch a machine in/out of maintenance mode

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
| [`on`](./managed-host-maintenance-on.md) | Put this machine into maintenance mode. Prevents an instance being assigned to it |
| [`off`](./managed-host-maintenance-off.md) | Return this machine to normal operation |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
