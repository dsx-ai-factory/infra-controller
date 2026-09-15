# `nico-admin-cli switch metadata remove-labels`

_[Hardware commands](../../hardware.md) › [switch](./switch.md) › [metadata](./switch-metadata.md) › **remove-labels**_

## NAME

nico-admin-cli-switch-metadata-remove-labels - Removes labels from the
Metadata of a Switch

## SYNOPSIS

**nico-admin-cli switch metadata remove-labels** \[**--keys**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\] \<*SWITCH*\>

## DESCRIPTION

Removes labels from the Metadata of a Switch

## OPTIONS

**--keys** *\<KEYS\>*  
The keys to remove

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

\<*SWITCH*\>  
The switch which should get updated metadata

## Examples

```sh
nico-admin-cli switch metadata remove-labels 12345678-1234-5678-90ab-cdef01234567 --keys env --keys edge
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
