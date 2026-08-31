# `nico-admin-cli machine-validation tests show`

*[Hardware commands](../../hardware.md) › [machine-validation](./machine-validation.md) › [tests](./machine-validation-tests.md) › **show***

## NAME

nico-admin-cli-machine-validation-tests-show - Show tests

## SYNOPSIS

**nico-admin-cli machine-validation tests show**
\[**-t**\|**--test-id**\] \[**-p**\|**--platforms**\]
\[**-c**\|**--contexts**\] \[**--show-un-verfied**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Show tests

## OPTIONS

**-t**, **--test-id** *\<TEST_ID\>*  
Unique identification of the test

**-p**, **--platforms** *\<PLATFORMS\>*  
List of platforms

**-c**, **--contexts** *\<CONTEXTS\>*  
List of contexts/tags

**--show-un-verfied**  
List unverfied tests also.

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
