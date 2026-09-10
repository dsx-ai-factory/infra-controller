# `nico-admin-cli machine-interfaces show`

_[Hardware commands](../../hardware.md) › [machine-interfaces](./machine-interfaces.md) › **show**_

## NAME

nico-admin-cli-machine-interfaces-show - List of all Machine interfaces

## SYNOPSIS

**nico-admin-cli machine-interfaces show** \[**-a**\|**--all**\]
\[**--more**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \[*INTERFACE_ID*\]

## DESCRIPTION

List of all Machine interfaces

## OPTIONS

**-a**, **--all**  
Show all machine interfaces (DEPRECATED)

**--more**  
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

\[*INTERFACE_ID*\]  
The interface ID to query, leave empty for all (default)

## Examples

```sh
nico-admin-cli machine-interfaces show
nico-admin-cli machine-interfaces show 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
