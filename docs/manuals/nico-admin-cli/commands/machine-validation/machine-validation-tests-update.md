# `nico-admin-cli machine-validation tests update`

_[Hardware commands](../../hardware.md) › [machine-validation](./machine-validation.md) › [tests](./machine-validation-tests.md) › **update**_

## NAME

nico-admin-cli-machine-validation-tests-update - Update existing test
case

## SYNOPSIS

**nico-admin-cli machine-validation tests update** \<**--test-id**\>
\<**--version**\> \[**--contexts**\] \[**--img-name**\]
\[**--execute-in-host**\] \[**--container-arg**\] \[**--description**\]
\[**--command**\] \[**--args**\] \[**--extra-err-file**\]
\[**--extra-output-file**\] \[**--external-config-file**\]
\[**--pre-condition**\] \[**--extended**\] \[**--timeout**\]
\[**--supported-platforms**\] \[**--custom-tags**\] \[**--components**\]
\[**--is-enabled**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Update existing test case

## OPTIONS

**--test-id** *\<TEST_ID\>*  
Unique identification of the test

**--version** *\<VERSION\>*  
Version to be verify

**--contexts** *\<CONTEXTS\>*  
List of contexts

**--img-name** *\<IMG_NAME\>*  
Container image name (must include @sha256:\<digest\>)

**--execute-in-host** *\<EXECUTE_IN_HOST\>*  
Run command using chroot in case of container\

\
*Possible values:*

- true

- false

**--container-arg** *\<CONTAINER_ARG\>*  
Container args

**--description** *\<DESCRIPTION\>*  
Description

**--command** *\<COMMAND\>*  
Command

**--args** *\<ARGS\>*  
Command args

**--extra-err-file** *\<EXTRA_ERR_FILE\>*  
Command output error file

**--extra-output-file** *\<EXTRA_OUTPUT_FILE\>*  
Command output file

**--external-config-file** *\<EXTERNAL_CONFIG_FILE\>*  
External file

**--pre-condition** *\<PRE_CONDITION\>*  
Pre condition

**--extended**  
Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

**--timeout** *\<TIMEOUT\>*  
Command Timeout

**--supported-platforms** *\<SUPPORTED_PLATFORMS\>*  
List of supported platforms

**--custom-tags** *\<CUSTOM_TAGS\>*  
List of custom tags

**--components** *\<COMPONENTS\>*  
List of system components

**--is-enabled** *\<IS_ENABLED\>*  
Enable the test\

\
*Possible values:*

- true

- false

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
