# `nico-admin-cli credential firmware-access-token`

_[Hardware commands](../../hardware.md) › [credential](./credential.md) › **firmware-access-token**_

## NAME

nico-admin-cli-credential-firmware-access-token - Manage firmware
artifact access tokens

## SYNOPSIS

**nico-admin-cli credential firmware-access-token** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Manage firmware artifact access tokens

## OPTIONS

**--extended**  
Extended result output.

This used by measured boot, where basic output contains just what you
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
| [`set`](./credential-firmware-access-token-set.md) | Set or rotate a firmware artifact access token |
| [`delete`](./credential-firmware-access-token-delete.md) | Delete a firmware artifact access token |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
