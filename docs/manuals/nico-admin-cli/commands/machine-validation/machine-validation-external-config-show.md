# `nico-admin-cli machine-validation external-config show`

*[Hardware commands](../../hardware.md) › [machine-validation](./machine-validation.md) › [external-config](./machine-validation-external-config.md) › **show***

## NAME

nico-admin-cli-machine-validation-external-config-show - Show External
config

## SYNOPSIS

**nico-admin-cli machine-validation external-config show**
\[**-n**\|**--name**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Show External config

## OPTIONS

**-n**, **--name** *\<NAME\>*  
Machine validation external config names

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

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
