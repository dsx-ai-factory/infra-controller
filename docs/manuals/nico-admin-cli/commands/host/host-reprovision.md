# `nico-admin-cli host reprovision`

*[Hardware commands](../../hardware.md) › [host](./host.md) › **reprovision***

## NAME

nico-admin-cli-host-reprovision - Host reprovisioning handling

## SYNOPSIS

**nico-admin-cli host reprovision** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Host reprovisioning handling

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

## Examples

```sh
nico-admin-cli host reprovision set --id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli host reprovision clear --id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli host reprovision list
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`set`](./host-reprovision-set.md) | Set the host in reprovisioning mode. |
| [`clear`](./host-reprovision-clear.md) | Clear the reprovisioning mode. |
| [`list`](./host-reprovision-list.md) | List all hosts pending reprovisioning. |
| [`mark-manual-upgrade-complete`](./host-reprovision-mark-manual-upgrade-complete.md) | Mark manual firmware upgrade as complete for a host. |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
