# `nico-admin-cli dev-env`

*[Admin commands](../../admin.md) › **dev-env***

## NAME

nico-admin-cli-dev-env - Dev Env related handling

## SYNOPSIS

**nico-admin-cli dev-env** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Dev Env related handling

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
| [`config`](./dev-env-config.md) | Config related handling |

---

**See also:** [Admin commands](../../admin.md) · [CLI reference index](../../index.md)
