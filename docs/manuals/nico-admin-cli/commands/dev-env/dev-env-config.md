# `nico-admin-cli dev-env config`

_[Admin commands](../../admin.md) › [dev-env](./dev-env.md) › **config**_

## NAME

nico-admin-cli-dev-env-config - Config related handling

## SYNOPSIS

**nico-admin-cli dev-env config** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Config related handling

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
| [`apply`](./dev-env-config-apply.md) | Apply devenv config |

---

**See also:** [Admin commands](../../admin.md) · [CLI reference index](../../README.md)
