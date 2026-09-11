# `nico-admin-cli tpm-ca show-unmatched-ek`

*[Hardware commands](../../hardware.md) › [tpm-ca](./tpm-ca.md) › **show-unmatched-ek***

## NAME

nico-admin-cli-tpm-ca-show-unmatched-ek - Show TPM EK certificates for
which there is no CA match

## SYNOPSIS

**nico-admin-cli tpm-ca show-unmatched-ek** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Show TPM EK certificates for which there is no CA match

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

## Examples

```sh
nico-admin-cli tpm-ca show-unmatched-ek
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
