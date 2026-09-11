# `nico-admin-cli dpf enable`

*[Hardware commands](../../hardware.md) › [dpf](./dpf.md) › **enable***

## NAME

nico-admin-cli-dpf-enable - Enable DPF

## SYNOPSIS

**nico-admin-cli dpf enable** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \[*HOST*\]

## DESCRIPTION

Enable DPF

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

\[*HOST*\]  
Host machine id

## Examples

```sh
nico-admin-cli dpf enable 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
