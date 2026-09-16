# `nico-admin-cli attestation spdm profile`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [spdm](./attestation-spdm.md) › **profile**_

## NAME

nico-admin-cli-attestation-spdm-profile - Manage the attestation policy
stored for each hardware class

## SYNOPSIS

**nico-admin-cli attestation spdm profile** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Manage the attestation policy stored for each hardware class

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
| [`list`](./attestation-spdm-profile-list.md) | List every stored attestation profile |
| [`get`](./attestation-spdm-profile-get.md) | Show the attestation profile for one hardware class |
| [`create`](./attestation-spdm-profile-create.md) | Store an attestation profile for a hardware class |
| [`update`](./attestation-spdm-profile-update.md) | Replace the selection of a stored attestation profile |
| [`delete`](./attestation-spdm-profile-delete.md) | Remove the attestation profile for a hardware class |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
