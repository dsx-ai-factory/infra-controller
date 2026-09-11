# `nico-admin-cli power-shelf health-report show`

*[Hardware commands](../../hardware.md) › [power-shelf](./power-shelf.md) › [health-report](./power-shelf-health-report.md) › **show***

## NAME

nico-admin-cli-power-shelf-health-report-show - List health report
sources for a power shelf

## SYNOPSIS

**nico-admin-cli power-shelf health-report show** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*POWER_SHELF_ID*\>

## DESCRIPTION

List health report sources for a power shelf

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

\<*POWER_SHELF_ID*\>  
Power Shelf ID to show health reports for

## Examples

```sh
nico-admin-cli power-shelf health-report show 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
