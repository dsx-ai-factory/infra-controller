# `nico-admin-cli machine-validation results show`

*[Hardware commands](../../hardware.md) › [machine-validation](./machine-validation.md) › [results](./machine-validation-results.md) › **show***

## NAME

nico-admin-cli-machine-validation-results-show - Show results

## SYNOPSIS

**nico-admin-cli machine-validation results show**
\[**-m**\|**--machine**\] \[**-v**\|**--validation-id**\]
\[**-t**\|**--test-name**\] \[**--history**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Show results

## OPTIONS

**-m**, **--machine** *\<MACHINE\>*  
Show machine validation result of a machine

**-v**, **--validation-id** *\<VALIDATION_ID\>*  
Machine validation id

**-t**, **--test-name** *\<TEST_NAME\>*  
Name of the test case

**--history**  
Results history

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
