# `nico-admin-cli tpm-ca add`

*[Hardware commands](../../hardware.md) › [tpm-ca](./tpm-ca.md) › **add***

## NAME

nico-admin-cli-tpm-ca-add - Add TPM CA certificate encoded in
DER/CER/PEM format in a given file

## SYNOPSIS

**nico-admin-cli tpm-ca add** \<**-f**\|**--filename**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Add TPM CA certificate encoded in DER/CER/PEM format in a given file

## OPTIONS

**-f**, **--filename** *\<FILENAME\>*  
File name containing certificate in DER format

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
nico-admin-cli tpm-ca add --filename /path/to/tpm-ca.der
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
