# `nico-admin-cli dpu agent-upgrade-policy`

_[Hardware commands](../../hardware.md) › [dpu](./dpu.md) › **agent-upgrade-policy**_

## NAME

nico-admin-cli-dpu-agent-upgrade-policy - Get or set forge-dpu-agent
upgrade policy

## SYNOPSIS

**nico-admin-cli dpu agent-upgrade-policy** \[**--set**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Get or set forge-dpu-agent upgrade policy

## OPTIONS

**--set** *\<SET\>*  
\
*Possible values:*

- off

- up-only

- up-down

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
nico-admin-cli dpu agent-upgrade-policy
nico-admin-cli dpu agent-upgrade-policy --set up-only
nico-admin-cli dpu agent-upgrade-policy --set up-down
nico-admin-cli dpu agent-upgrade-policy --set off
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
