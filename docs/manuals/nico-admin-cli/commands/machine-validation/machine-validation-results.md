# `nico-admin-cli machine-validation results`

*[Hardware commands](../../hardware.md) › [machine-validation](./machine-validation.md) › **results***

## NAME

nico-admin-cli-machine-validation-results - Display machine validation
results of individual runs

## SYNOPSIS

**nico-admin-cli machine-validation results** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Display machine validation results of individual runs

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
nico-admin-cli machine-validation results show --machine 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli machine-validation results show --validation-id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli machine-validation results show --validation-id 12345678-1234-5678-90ab-cdef01234567 --test-name gpu_bandwidth --history
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`show`](./machine-validation-results-show.md) | Show results |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
