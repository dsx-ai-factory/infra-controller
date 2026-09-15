# `nico-admin-cli dpu-remediation create`

_[Hardware commands](../../hardware.md) › [dpu-remediation](./dpu-remediation.md) › **create**_

## NAME

nico-admin-cli-dpu-remediation-create - Create a remediation

## SYNOPSIS

**nico-admin-cli dpu-remediation create** \<**--script-filename**\>
\[**--retries**\] \[**--meta-name**\] \[**--meta-description**\]
\[**--label**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Create a remediation

## OPTIONS

**--script-filename** *\<SCRIPT_FILENAME\>*  
The filename of the script to run

**--retries** *\<RETRIES\>*  
specify the amount of retries for the remediation, defaults to no
retries

**--meta-name** *\<META_NAME\>*  
The name that should be used as part of the Metadata for newly created
Remediations. Completely optional.

**--meta-description** *\<META_DESCRIPTION\>*  
The description that should be used as part of the Metadata for newly
created Remediations. Completely optional.

**--label** *\<LABEL\>*  
A label that will be added as metadata for the newly created
Remediation. The labels key and value must be separated by a :
character. E.g. DATACENTER:XYZ. Completely optional.

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

## Examples

```sh
nico-admin-cli dpu-remediation create --script-filename ./remediate.sh
nico-admin-cli dpu-remediation create --script-filename ./remediate.sh --retries 3
nico-admin-cli dpu-remediation create --script-filename ./remediate.sh --meta-name "clear-eeprom" --meta-description "Clears stale EEPROM state" --label DATACENTER:XYZ
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
