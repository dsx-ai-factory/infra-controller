# `nico-admin-cli vpc show`

*[Network commands](../../network.md) › [vpc](./vpc.md) › **show***

## NAME

nico-admin-cli-vpc-show - Display VPC information

## SYNOPSIS

**nico-admin-cli vpc show** \[**-t**\|**--tenant-org-id**\]
\[**-n**\|**--name**\] \[**--label-key**\] \[**--label-value**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\] \[*ID*\]

## DESCRIPTION

Display VPC information

## OPTIONS

**-t**, **--tenant-org-id** *\<TENANT_ORG_ID\>*  
The Tenant Org ID to query

**-n**, **--name** *\<NAME\>*  
The VPC name to query

**--label-key** *\<LABEL_KEY\>*  
The key of VPC label to query

**--label-value** *\<LABEL_VALUE\>*  
The value of VPC label to query

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

\[*ID*\]  
The VPC ID to query, leave empty for all (default)

## Examples

```sh
nico-admin-cli vpc show
nico-admin-cli vpc show 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli vpc show --tenant-org-id fds34511233a
nico-admin-cli vpc show --label-key env --label-value prod
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
