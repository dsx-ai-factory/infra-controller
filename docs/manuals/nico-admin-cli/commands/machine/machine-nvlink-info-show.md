# `nico-admin-cli machine nvlink-info show`

_[Hardware commands](../../hardware.md) › [machine](./machine.md) › [nvlink-info](./machine-nvlink-info.md) › **show**_

## NAME

nico-admin-cli-machine-nvlink-info-show - Show existing NVLink info

## SYNOPSIS

**nico-admin-cli machine nvlink-info show** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*MACHINE_ID*\>

## DESCRIPTION

Show existing NVLink info

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

\<*MACHINE_ID*\>  
Machine ID to query

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
