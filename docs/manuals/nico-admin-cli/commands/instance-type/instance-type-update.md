# `nico-admin-cli instance-type update`

*[Tenant commands](../../tenant.md) › [instance-type](./instance-type.md) › **update***

## NAME

nico-admin-cli-instance-type-update - Update an instance type

## SYNOPSIS

**nico-admin-cli instance-type update** \<**-i**\|**--id**\>
\[**-n**\|**--name**\] \[**-d**\|**--description**\]
\[**-l**\|**--labels**\] \[**-f**\|**--desired-capabilities**\]
\[**-v**\|**--version**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Update an instance type

## OPTIONS

**-i**, **--id** *\<ID\>*  
Instance type ID to update

**-n**, **--name** *\<NAME\>*  
Name of the instance type

**-d**, **--description** *\<DESCRIPTION\>*  
Description of the instance type

**-l**, **--labels** *\<LABELS\>*  
JSON map of simple key:value pairs to be applied as labels to the
instance type - will COMPLETELY overwrite any existing labels

**-f**, **--desired-capabilities** *\<DESIRED_CAPABILITIES\>*  
Optional, JSON array containing a set of instance type capability
filters - will COMPLETELY overwrite any existing filters

**-v**, **--version** *\<VERSION\>*  
Optional, version to use for comparison when performing the update,
which will be rejected if the actual version of the record does not
match the value of this parameter

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
nico-admin-cli instance-type update --id 12345678-1234-5678-90ab-cdef01234567 --name dgx-h100-640gb
nico-admin-cli instance-type update --id 12345678-1234-5678-90ab-cdef01234567 --labels '{"tier":"premium"}' --desired-capabilities '[{"key":"gpu_count","value":"8"}]'
nico-admin-cli instance-type update --id 12345678-1234-5678-90ab-cdef01234567 --description "DGX H100 640GB" --version 3
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
