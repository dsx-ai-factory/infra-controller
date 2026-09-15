# `nico-admin-cli set`

_[Hardware commands](../../hardware.md) › **set**_

## NAME

nico-admin-cli-set - Set carbide-api dynamic features

## SYNOPSIS

**nico-admin-cli set** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Set carbide-api dynamic features

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
| [`log-filter`](./set-log-filter.md) | Set RUST_LOG |
| [`create-machines`](./set-create-machines.md) | Set create_machines |
| [`site-explorer`](./set-site-explorer.md) | Enable or disable site-explorer |
| [`bmc-proxy`](./set-bmc-proxy.md) | Set bmc_proxy |
| [`tracing-enabled`](./set-tracing-enabled.md) | Configure whether trace/span information is sent to an OTLP endpoint like Tempo |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
