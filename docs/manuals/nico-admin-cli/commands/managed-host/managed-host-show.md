# `nico-admin-cli managed-host show`

_[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › **show**_

## NAME

nico-admin-cli-managed-host-show - Display managed host information

## SYNOPSIS

**nico-admin-cli managed-host show** \[**--help**\]
\[**-a**\|**--all**\] \[**-i**\|**--ips**\]
\[**-t**\|**--instance-type-id**\] \[**-m**\|**--more**\] \[**--fix**\]
\[**--quarantine**\] \[**--max-width**\] \[**--extended**\]
\[**--sort-by**\] \[*MACHINE*\]

## DESCRIPTION

Display managed host information

## OPTIONS

**--help**  
**-a**, **--all**  
Show all managed hosts (DEPRECATED)

**-i**, **--ips**  
Show IP details in summary

**-t**, **--instance-type-id** *\<INSTANCE_TYPE_ID\>*  
Show only hosts for this instance type

**-m**, **--more**  
Show GPU and memory details in summary

**--fix**  
Show only hosts in maintenance mode

**--quarantine**  
Show only hosts in quarantine

**--max-width** *\<\[COLUMN=\]WIDTH\>*  
Limit displayed column width to WIDTH characters, truncating longer
values with an ellipsis ('...'). A column never narrows below its
header's width, so WIDTH is an upper bound on values, not a guaranteed
rendered width: a WIDTH shorter than the header still lets values fill
the header's width for free, and the ellipsis is only added when that
effective width (WIDTH, or the header's length if longer) exceeds 3
characters; at 3 or fewer there's no room for one, so the value is
truncated without it. WIDTH 0 means no limit (the same as not
specifying that column at all; useful as COLUMN=0 to exempt one column
from a blanket --max-width). Repeatable. A bare WIDTH applies to every
column; COLUMN=WIDTH limits just that column, where COLUMN must exactly
match the columns displayed header text (case-insensitive), e.g.
State=40. For a header containing spaces, quote the whole COLUMN=WIDTH
argument, e.g. "Machine IDs (H/D)=40". An unmatched COLUMN is ignored
with a warning listing the valid headers for this invocation.

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

\[*MACHINE*\]  
Show managed host specific details (using host or dpu machine id), leave
empty for all

## Examples

```sh
nico-admin-cli managed-host show
nico-admin-cli managed-host show 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli managed-host show --ips
nico-admin-cli managed-host show --max-width State=40
nico-admin-cli managed-host show --max-width "Machine IDs (H/D)=40"
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
