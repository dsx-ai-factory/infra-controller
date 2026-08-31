# `nico-admin-cli tpm-ca`

*[Hardware commands](../../hardware.md) › **tpm-ca***

## NAME

nico-admin-cli-tpm-ca - Manage TPM CA certificates

## SYNOPSIS

**nico-admin-cli tpm-ca** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Manage TPM CA certificates

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
| [`show`](./tpm-ca-show.md) | Show all TPM CA certificates |
| [`delete`](./tpm-ca-delete.md) | Delete TPM CA certificate with a given id |
| [`add`](./tpm-ca-add.md) | Add TPM CA certificate encoded in DER/CER/PEM format in a given file |
| [`show-unmatched-ek`](./tpm-ca-show-unmatched-ek.md) | Show TPM EK certificates for which there is no CA match |
| [`add-bulk`](./tpm-ca-add-bulk.md) | Add all certificates in a dir as CA certificates |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
