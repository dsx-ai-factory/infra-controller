# `nico-admin-cli attestation measured-boot report`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › **report***

## NAME

nico-admin-cli-attestation-measured-boot-report - Work with machine
reports

## SYNOPSIS

**nico-admin-cli attestation measured-boot report** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Work with machine reports

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
| [`create`](./attestation-measured-boot-report-create.md) | Create a new report with a given config. |
| [`delete`](./attestation-measured-boot-report-delete.md) | Delete a report by ID. |
| [`promote`](./attestation-measured-boot-report-promote.md) | Promote a specific journal entry to an active bundle |
| [`revoke`](./attestation-measured-boot-report-revoke.md) | Mark a specific journal entry as a revoked bundle. |
| [`show`](./attestation-measured-boot-report-show.md) | Show reports in different ways. |
| [`list`](./attestation-measured-boot-report-list.md) | List reports by various ways. |
| [`match`](./attestation-measured-boot-report-match.md) | Match reports with the provided PCR register values. |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
