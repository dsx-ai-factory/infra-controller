# `nico-admin-cli set tracing-enabled`

_[Hardware commands](../../hardware.md) › [set](./set.md) › **tracing-enabled**_

## NAME

nico-admin-cli-set-tracing-enabled - Configure whether trace/span
information is sent to an OTLP endpoint like Tempo

## SYNOPSIS

**nico-admin-cli set tracing-enabled** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*true\|false*\>

## DESCRIPTION

Configure whether trace/span information is sent to an OTLP endpoint
like Tempo

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

\<*true\|false*\>  
\
*Possible values:*

- true

- false

## Examples

```sh
nico-admin-cli set tracing-enabled true
nico-admin-cli set tracing-enabled false
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
