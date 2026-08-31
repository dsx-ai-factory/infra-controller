# `nico-admin-cli attestation measured-boot machine`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › **machine***

## NAME

nico-admin-cli-attestation-measured-boot-machine - Work with
mock-machine entries

## SYNOPSIS

**nico-admin-cli attestation measured-boot machine** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Work with mock-machine entries

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
| [`attest`](./attestation-measured-boot-machine-attest.md) | Send measurements for a machine. |
| [`show`](./attestation-measured-boot-machine-show.md) | Get all info about a machine. |
| [`list`](./attestation-measured-boot-machine-list.md) | List all machines + their info. |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
