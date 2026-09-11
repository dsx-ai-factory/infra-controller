# `nico-admin-cli machine hardware-info show`

*[Hardware commands](../../hardware.md) › [machine](./machine.md) › [hardware-info](./machine-hardware-info.md) › **show***

## NAME

nico-admin-cli-machine-hardware-info-show - Show the hardware info of
the machine

## SYNOPSIS

**nico-admin-cli machine hardware-info show** \<**--machine**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Show the hardware info of the machine

## OPTIONS

**--machine** *\<MACHINE\>*  
Show the hardware info of this Machine ID

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

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
