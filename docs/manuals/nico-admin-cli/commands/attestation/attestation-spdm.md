# `nico-admin-cli attestation spdm`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › **spdm**_

## NAME

nico-admin-cli-attestation-spdm - Perform SPDM attestation

## SYNOPSIS

**nico-admin-cli attestation spdm** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Perform SPDM attestation

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
| [`cancel`](./attestation-spdm-cancel.md) | Cancel attestation for a given machine id |
| [`get`](./attestation-spdm-get.md) | Get SPDM attestation details for a given machine id |
| [`list`](./attestation-spdm-list.md) | List SPDM attestation machine statuses |
| [`trigger`](./attestation-spdm-trigger.md) | Trigger attestation for a given machine with id |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
