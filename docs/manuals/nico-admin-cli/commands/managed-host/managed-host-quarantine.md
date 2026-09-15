# `nico-admin-cli managed-host quarantine`

_[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › **quarantine**_

## NAME

nico-admin-cli-managed-host-quarantine - Quarantine a host (disabling
network access on host)

## SYNOPSIS

**nico-admin-cli managed-host quarantine** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Quarantine a host (disabling network access on host)

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
| [`on`](./managed-host-quarantine-on.md) | Put this machine into quarantine. Prevents any network access on the host machine |
| [`off`](./managed-host-quarantine-off.md) | Take this machine out of quarantine |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
