# `nico-admin-cli trim-table measured-boot`

*[Hardware commands](../../hardware.md) › [trim-table](./trim-table.md) › **measured-boot***

## NAME

nico-admin-cli-trim-table-measured-boot

## SYNOPSIS

**nico-admin-cli trim-table measured-boot** \<**--keep-entries**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

## OPTIONS

**--keep-entries** *\<KEEP_ENTRIES\>*  
Number of entries to keep

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
nico-admin-cli trim-table measured-boot --keep-entries 1000
nico-admin-cli trim-table measured-boot --keep-entries 1
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
