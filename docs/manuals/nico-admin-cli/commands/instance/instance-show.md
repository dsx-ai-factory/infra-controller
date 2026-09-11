# `nico-admin-cli instance show`

*[Tenant commands](../../tenant.md) › [instance](./instance.md) › **show***

## NAME

nico-admin-cli-instance-show - Display instance information

## SYNOPSIS

**nico-admin-cli instance show** \[**-e**\|**--extrainfo**\]
\[**-t**\|**--tenant-org-id**\] \[**-v**\|**--vpc-id**\]
\[**--label-key**\] \[**--label-value**\] \[**--instance-type-id**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\] \[*ID*\]

## DESCRIPTION

Display instance information

## OPTIONS

**-e**, **--extrainfo**  
**-t**, **--tenant-org-id** *\<TENANT_ORG_ID\>*  
The Tenant Org ID to query

**-v**, **--vpc-id** *\<VPC_ID\>*  
The VPC ID to query.

**--label-key** *\<LABEL_KEY\>*  
The key of label instance to query

**--label-value** *\<LABEL_VALUE\>*  
The value of label instance to query

**--instance-type-id** *\<INSTANCE_TYPE_ID\>*  
The instance type ID to query.

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

\[*ID*\] \[default: \]  
The instance ID to query, leave empty for all (default)

## Examples

```sh
nico-admin-cli instance show
nico-admin-cli instance show 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli instance show --tenant-org-id fds34511233a
nico-admin-cli instance show --vpc-id abcdef01-2345-6789-abcd-ef0123456789
nico-admin-cli instance show --label-key role --label-value training
nico-admin-cli instance show --instance-type-id 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
