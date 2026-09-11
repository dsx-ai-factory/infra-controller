# `nico-admin-cli dpu-remediation`

*[Hardware commands](../../hardware.md) › **dpu-remediation***

## NAME

nico-admin-cli-dpu-remediation - Dpu Remediation handling

## SYNOPSIS

**nico-admin-cli dpu-remediation** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Dpu Remediation handling

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
| [`create`](./dpu-remediation-create.md) | Create a remediation |
| [`approve`](./dpu-remediation-approve.md) | Approve a remediation |
| [`revoke`](./dpu-remediation-revoke.md) | Revoke a remediation |
| [`enable`](./dpu-remediation-enable.md) | Enable a remediation |
| [`disable`](./dpu-remediation-disable.md) | Disable a remediation |
| [`show`](./dpu-remediation-show.md) | Display remediation information |
| [`list-applied`](./dpu-remediation-list-applied.md) | Display information about applied remediations |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
