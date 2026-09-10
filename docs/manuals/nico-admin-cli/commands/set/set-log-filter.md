# `nico-admin-cli set log-filter`

_[Hardware commands](../../hardware.md) › [set](./set.md) › **log-filter**_

## NAME

nico-admin-cli-set-log-filter - Set RUST_LOG

## SYNOPSIS

**nico-admin-cli set log-filter** \<**-f**\|**--filter**\>
\[**--expiry**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Set RUST_LOG

## OPTIONS

**-f**, **--filter** *\<FILTER\>*  
Set servers RUST_LOG.

**--expiry** *\<EXPIRY\>* \[default: 1h\]  
Revert to startup RUST_LOG after this much time, friendly format e.g.
1h, 3min, https://docs.rs/duration-str/latest/duration_str/

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
nico-admin-cli set log-filter --filter debug
nico-admin-cli set log-filter --filter carbide_api=trace,info --expiry 30min
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
