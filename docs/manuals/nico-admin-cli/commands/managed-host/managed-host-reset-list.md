# `nico-admin-cli managed-host reset list`

_[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › [reset](./managed-host-reset.md) › **list**_

## NAME

nico-admin-cli-managed-host-reset-list - List all managed hosts pending
reset.

## SYNOPSIS

**nico-admin-cli managed-host reset list** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

List all managed hosts pending reset.

## OPTIONS

**--extended**  
Extended result output.

This used by measured boot, where basic output contains just what you
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

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
