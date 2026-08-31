# `nico-admin-cli machine-validation`

*[Hardware commands](../../hardware.md) › **machine-validation***

## NAME

nico-admin-cli-machine-validation - Machine Validation

## SYNOPSIS

**nico-admin-cli machine-validation** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Machine Validation

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
| [`external-config`](./machine-validation-external-config.md) | External config |
| [`on-demand`](./machine-validation-on-demand.md) | Ondemand Validation |
| [`results`](./machine-validation-results.md) | Display machine validation results of individual runs |
| [`runs`](./machine-validation-runs.md) | Display all machine validation runs |
| [`tests`](./machine-validation-tests.md) | Supported Tests |
| [`plugins`](./machine-validation-plugins.md) | Manage OCI Machine Validation plugins |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
