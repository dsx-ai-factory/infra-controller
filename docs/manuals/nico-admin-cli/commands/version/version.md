# `nico-admin-cli version`

_[Admin commands](../../admin.md) › **version**_

## NAME

nico-admin-cli-version - Print API server version

## SYNOPSIS

**nico-admin-cli version** \[**-s**\|**--show-runtime-config**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Print API server version

## OPTIONS

**-s**, **--show-runtime-config**  
Display Runtime Config also.

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
nico-admin-cli version
nico-admin-cli version --show-runtime-config
```

---

**See also:** [Admin commands](../../admin.md) · [CLI reference index](../../README.md)
