# `nico-admin-cli tpm-ca show`

_[Hardware commands](../../hardware.md) › [tpm-ca](./tpm-ca.md) › **show**_

## NAME

nico-admin-cli-tpm-ca-show - Show all TPM CA certificates

## SYNOPSIS

**nico-admin-cli tpm-ca show** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Show all TPM CA certificates

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

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
