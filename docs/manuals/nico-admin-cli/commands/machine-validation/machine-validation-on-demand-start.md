# `nico-admin-cli machine-validation on-demand start`

_[Hardware commands](../../hardware.md) › [machine-validation](./machine-validation.md) › [on-demand](./machine-validation-on-demand.md) › **start**_

## NAME

nico-admin-cli-machine-validation-on-demand-start - Start on demand
machine validation

## SYNOPSIS

**nico-admin-cli machine-validation on-demand start** \[**--help**\]
\<**-m**\|**--machine**\> \[**--tags**\] \[**--allowed-tests**\]
\[**--run-unverified-tests**\] \[**--contexts**\] \[**--extended**\]
\[**--sort-by**\]

## DESCRIPTION

Start on demand machine validation

## OPTIONS

**--help**  
**-m**, **--machine** *\<MACHINE\>*  
Machine id for start validation

**--tags** *\<TAGS\>*  
Results history

**--allowed-tests** *\<ALLOWED_TESTS\>*  
Allowed tests

**--run-unverified-tests**  
Run unverified tests

**--contexts** *\<CONTEXTS\>*  
Contexts

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

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
