# `nico-admin-cli tpm-ca delete`

*[Hardware commands](../../hardware.md) › [tpm-ca](./tpm-ca.md) › **delete***

## NAME

nico-admin-cli-tpm-ca-delete - Delete TPM CA certificate with a given id

## SYNOPSIS

**nico-admin-cli tpm-ca delete** \<**-c**\|**--ca-id**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Delete TPM CA certificate with a given id

## OPTIONS

**-c**, **--ca-id** *\<CA_ID\>*  
TPM CA id obtained from the show command

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
nico-admin-cli tpm-ca delete --ca-id 42
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
