# `nico-admin-cli machine-validation runs`

_[Hardware commands](../../hardware.md) › [machine-validation](./machine-validation.md) › **runs**_

## NAME

nico-admin-cli-machine-validation-runs - Display all machine validation
runs

## SYNOPSIS

**nico-admin-cli machine-validation runs** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Display all machine validation runs

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
nico-admin-cli machine-validation runs show
nico-admin-cli machine-validation runs show --machine 12345678-1234-5678-90ab-cdef01234567 --history
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`show`](./machine-validation-runs-show.md) | Show Runs |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
