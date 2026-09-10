# `nico-admin-cli host`

_[Hardware commands](../../hardware.md) › **host**_

## NAME

nico-admin-cli-host - Host specific handling

## SYNOPSIS

**nico-admin-cli host** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Host specific handling

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
| [`set-uefi-password`](./host-set-uefi-password.md) | Set Host UEFI password |
| [`clear-uefi-password`](./host-clear-uefi-password.md) | Clear Host UEFI password |
| [`generate-host-uefi-password`](./host-generate-host-uefi-password.md) | Generates a string that can be a site-default host UEFI password in Vault |
| [`reprovision`](./host-reprovision.md) | Host reprovisioning handling |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
