# `nico-admin-cli machine metadata show`

*[Hardware commands](../../hardware.md) › [machine](./machine.md) › [metadata](./machine-metadata.md) › **show***

## NAME

nico-admin-cli-machine-metadata-show - Show the Metadata of the Machine

## SYNOPSIS

**nico-admin-cli machine metadata show** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*MACHINE*\>

## DESCRIPTION

Show the Metadata of the Machine

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

\<*MACHINE*\>  
The machine which should get updated metadata

## Examples

```sh
nico-admin-cli machine metadata show 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
