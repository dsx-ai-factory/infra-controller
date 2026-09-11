# `nico-admin-cli secrets`

_[Admin commands](../../admin.md) › **secrets**_

## NAME

nico-admin-cli-secrets - Secrets management

## SYNOPSIS

**nico-admin-cli secrets** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Secrets management

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
| [`re-wrap`](./secrets-re-wrap.md) | Re-wrap secret DEKs to use the currently active KEK per routing config |

---

**See also:** [Admin commands](../../admin.md) · [CLI reference index](../../README.md)
