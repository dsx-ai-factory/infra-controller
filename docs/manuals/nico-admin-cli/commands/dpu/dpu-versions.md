# `nico-admin-cli dpu versions`

*[Hardware commands](../../hardware.md) › [dpu](./dpu.md) › **versions***

## NAME

nico-admin-cli-dpu-versions - View DPU firmware status

## SYNOPSIS

**nico-admin-cli dpu versions** \[**-u**\|**--updates-only**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

View DPU firmware status

## OPTIONS

**-u**, **--updates-only**  
Only show DPUs that need upgrades

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
nico-admin-cli dpu versions
nico-admin-cli dpu versions --updates-only
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
