# `nico-admin-cli credential delete-ufm`

_[Hardware commands](../../hardware.md) › [credential](./credential.md) › **delete-ufm**_

## NAME

nico-admin-cli-credential-delete-ufm - Delete UFM credential

## SYNOPSIS

**nico-admin-cli credential delete-ufm** \<**--url**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Delete UFM credential

## OPTIONS

**--url** *\<URL\>*  
The UFM url

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
nico-admin-cli credential delete-ufm --url https://192.0.2.10
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
