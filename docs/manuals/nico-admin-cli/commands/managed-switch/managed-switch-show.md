# `nico-admin-cli managed-switch show`

*[Hardware commands](../../hardware.md) › [managed-switch](./managed-switch.md) › **show***

## NAME

nico-admin-cli-managed-switch-show - Display managed switch information

## SYNOPSIS

**nico-admin-cli managed-switch show** \[**-i**\|**--ips**\]
\[**-m**\|**--more**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \[*IDENTIFIER*\]

## DESCRIPTION

Display managed switch information

## OPTIONS

**-i**, **--ips**  
Show BMC/NVOS MAC details in summary

**-m**, **--more**  
Show serial, power, and health details in summary

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

\[*IDENTIFIER*\]  
Switch ID or name to show details for (leave empty for all)

## Examples

```sh
nico-admin-cli managed-switch show
nico-admin-cli managed-switch show 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
