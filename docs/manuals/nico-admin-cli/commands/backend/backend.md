# `nico-admin-cli backend`

_[Hardware commands](../../hardware.md) › **backend**_

## NAME

nico-admin-cli-backend - NICo backend service operations

## SYNOPSIS

**nico-admin-cli backend** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<COMMAND\>

## DESCRIPTION

NICo backend service operations

Operations targeting NICo backend services directly, routed through the
nico-api Forge RPC.

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

## Subcommands

| Subcommand | Description |
|---|---|
| [`rms`](./rms/backend-rms.md) | RMS backend operations |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
