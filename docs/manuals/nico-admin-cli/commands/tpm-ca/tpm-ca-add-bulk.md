# `nico-admin-cli tpm-ca add-bulk`

_[Hardware commands](../../hardware.md) › [tpm-ca](./tpm-ca.md) › **add-bulk**_

## NAME

nico-admin-cli-tpm-ca-add-bulk - Add all certificates in a dir as CA
certificates

## SYNOPSIS

**nico-admin-cli tpm-ca add-bulk** \<**-d**\|**--dirname**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Add all certificates in a dir as CA certificates

## OPTIONS

**-d**, **--dirname** *\<DIRNAME\>*  
Directory path containing all CA certs

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
nico-admin-cli tpm-ca add-bulk --dirname /path/to/tpm-ca-certs/
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
