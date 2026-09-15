# `nico-admin-cli backend rms`

_[Hardware commands](../../hardware.md) › [backend](./backend.md) › **rms**_

## NAME

nico-admin-cli-backend-rms - RMS backend operations

## SYNOPSIS

**nico-admin-cli backend rms** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<COMMAND\>

## DESCRIPTION

RMS backend operations

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
| [`status`](./backend-rms-status.md) | Probe the RMS backend connectivity and version |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
