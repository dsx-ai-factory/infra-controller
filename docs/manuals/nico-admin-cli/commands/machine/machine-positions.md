# `nico-admin-cli machine positions`

*[Hardware commands](../../hardware.md) › [machine](./machine.md) › **positions***

## NAME

nico-admin-cli-machine-positions - Show physical location info for
machines in rack-based systems

## SYNOPSIS

**nico-admin-cli machine positions** \[**-m**\|**--machine**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Show physical location info for machines in rack-based systems.

Returns rack topology information including: - Physical slot number: The
slot position in the rack - Compute tray index: The compute tray
containing this machine - Topology ID: Identifier for the rack topology
configuration - Revision ID: Hardware revision identifier - Switch ID:
Associated network switch - Power shelf ID: Associated power shelf

## OPTIONS

**-m**, **--machine** \[*\<MACHINE\>...*\]  
The machine(s) to query, leave empty for all (default)

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
nico-admin-cli machine positions
nico-admin-cli machine positions --machine 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
