# `nico-admin-cli credential generate-ufm-cert`

*[Hardware commands](../../hardware.md) › [credential](./credential.md) › **generate-ufm-cert***

## NAME

nico-admin-cli-credential-generate-ufm-cert - Generate UFM credential

## SYNOPSIS

**nico-admin-cli credential generate-ufm-cert** \[**--fabric**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Generate UFM credential

## OPTIONS

**--fabric** *\<FABRIC\>* \[default: default\]  
Infiniband fabric.

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
nico-admin-cli credential generate-ufm-cert
nico-admin-cli credential generate-ufm-cert --fabric default
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
