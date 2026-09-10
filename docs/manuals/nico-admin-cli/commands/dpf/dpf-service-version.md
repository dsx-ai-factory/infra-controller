# `nico-admin-cli dpf service-version`

_[Hardware commands](../../hardware.md) › [dpf](./dpf.md) › **service-version**_

## NAME

nico-admin-cli-dpf-service-version - Compare configured vs deployed DPF
service versions

## SYNOPSIS

**nico-admin-cli dpf service-version** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Compare configured vs deployed DPF service versions

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
nico-admin-cli dpf service-version
nico-admin-cli dpf sv
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
