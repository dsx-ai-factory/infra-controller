# `nico-admin-cli managed-switch list`

*[Hardware commands](../../hardware.md) › [managed-switch](./managed-switch.md) › **list***

## NAME

nico-admin-cli-managed-switch-list - List all managed switches

## SYNOPSIS

**nico-admin-cli managed-switch list** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

List all managed switches

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
nico-admin-cli managed-switch list
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
