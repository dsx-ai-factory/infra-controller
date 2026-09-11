# `nico-admin-cli managed-host start-updates`

*[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › **start-updates***

## NAME

nico-admin-cli-managed-host-start-updates - Start updates for machines
with delayed updates, such as GB200

## SYNOPSIS

**nico-admin-cli managed-host start-updates** \<**--machines**\>
\[**--start**\] \[**--end**\] \[**--cancel**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Start updates for machines with delayed updates, such as GB200

## OPTIONS

**--machines** *\<MACHINES\>...*  
Machine IDs to update, space separated

**--start** *\<START\>*  
Start of the maintenance window for doing the updates (default now)
format 2025-01-02T03:04:05+0000 or 2025-01-02T03:04:05 for local time

**--end** *\<END\>*  
End of starting new updates (default 24 hours from the start) format
2025-01-02T03:04:05+0000 or 2025-01-02T03:04:05 for local time

**--cancel**  
Cancel any new updates

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
nico-admin-cli managed-host start-updates --machines 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli managed-host start-updates --machines 12345678-1234-5678-90ab-cdef01234567 --start 2026-01-02T03:04:05
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
