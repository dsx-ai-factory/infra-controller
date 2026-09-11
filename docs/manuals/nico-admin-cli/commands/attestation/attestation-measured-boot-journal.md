# `nico-admin-cli attestation measured-boot journal`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › **journal**_

## NAME

nico-admin-cli-attestation-measured-boot-journal - Work with machine
meausrement journals

## SYNOPSIS

**nico-admin-cli attestation measured-boot journal** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Work with machine meausrement journals

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
| [`delete`](./attestation-measured-boot-journal-delete.md) | Delete a journal entry. |
| [`show`](./attestation-measured-boot-journal-show.md) | Show a journal entry by ID, or all. |
| [`list`](./attestation-measured-boot-journal-list.md) | List all journal IDs and machines. |
| [`promote`](./attestation-measured-boot-journal-promote.md) | Promote a journal entry report to a bundle. |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
