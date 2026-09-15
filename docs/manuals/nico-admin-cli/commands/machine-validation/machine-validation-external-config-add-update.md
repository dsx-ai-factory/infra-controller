# `nico-admin-cli machine-validation external-config add-update`

_[Hardware commands](../../hardware.md) › [machine-validation](./machine-validation.md) › [external-config](./machine-validation-external-config.md) › **add-update**_

## NAME

nico-admin-cli-machine-validation-external-config-add-update - Update
External config

## SYNOPSIS

**nico-admin-cli machine-validation external-config add-update**
\<**-f**\|**--file-name**\> \<**-n**\|**--name**\>
\<**-d**\|**--description**\> \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Update External config

## OPTIONS

**-f**, **--file-name** *\<FILE_NAME\>*  
Name of the file to update

**-n**, **--name** *\<NAME\>*  
Name of the config

**-d**, **--description** *\<DESCRIPTION\>*  
description of the file to update

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

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
