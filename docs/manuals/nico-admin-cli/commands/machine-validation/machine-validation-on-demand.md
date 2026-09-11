# `nico-admin-cli machine-validation on-demand`

*[Hardware commands](../../hardware.md) › [machine-validation](./machine-validation.md) › **on-demand***

## NAME

nico-admin-cli-machine-validation-on-demand - Ondemand Validation

## SYNOPSIS

**nico-admin-cli machine-validation on-demand** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Ondemand Validation

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

## Examples

```sh
nico-admin-cli machine-validation on-demand start --machine 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli machine-validation on-demand start --machine 12345678-1234-5678-90ab-cdef01234567 --allowed-tests gpu_bandwidth --run-unverified-tests
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`start`](./machine-validation-on-demand-start.md) | Start on demand machine validation |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
