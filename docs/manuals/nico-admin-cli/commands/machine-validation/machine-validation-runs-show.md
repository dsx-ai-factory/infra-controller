# `nico-admin-cli machine-validation runs show`

*[Hardware commands](../../hardware.md) › [machine-validation](./machine-validation.md) › [runs](./machine-validation-runs.md) › **show***

## NAME

nico-admin-cli-machine-validation-runs-show - Show Runs

## SYNOPSIS

**nico-admin-cli machine-validation runs show**
\[**-m**\|**--machine**\] \[**--history**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Show Runs

## OPTIONS

**-m**, **--machine** *\<MACHINE\>*  
Show machine validation runs of a machine

**--history**  
run history

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
