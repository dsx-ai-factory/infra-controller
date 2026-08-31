# `nico-admin-cli machine-validation tests enable`

*[Hardware commands](../../hardware.md) › [machine-validation](./machine-validation.md) › [tests](./machine-validation-tests.md) › **enable***

## NAME

nico-admin-cli-machine-validation-tests-enable - Enabled a test

## SYNOPSIS

**nico-admin-cli machine-validation tests enable**
\<**-t**\|**--test-id**\> \<**-v**\|**--version**\> \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Enabled a test

## OPTIONS

**-t**, **--test-id** *\<TEST_ID\>*  
Unique identification of the test

**-v**, **--version** *\<VERSION\>*  
Version to be verify

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
