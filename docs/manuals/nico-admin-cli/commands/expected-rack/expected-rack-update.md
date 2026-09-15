# `nico-admin-cli expected-rack update`

_[Tenant commands](../../tenant.md) › [expected-rack](./expected-rack.md) › **update**_

## NAME

nico-admin-cli-expected-rack-update - Update expected rack

## SYNOPSIS

**nico-admin-cli expected-rack update** \[**--rack-profile-id**\]
\[**--meta-name**\] \[**--meta-description**\] \[**--label**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*RACK_ID*\>

## DESCRIPTION

Update expected rack

## OPTIONS

**--rack-profile-id** *\<RACK_PROFILE_ID\>*  
Rack profile ID of the expected rack

**--meta-name** *\<META_NAME\>*  
The name that should be used as part of the Metadata for newly created
Rack. If empty, the Rack Id will be used

**--meta-description** *\<META_DESCRIPTION\>*  
The description that should be used as part of the Metadata for newly
created Rack

**--label** *\<LABEL\>*  
A label that will be added as metadata for the newly created Rack. The
labels key and value must be separated by a : character

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

\<*RACK_ID*\>  
Rack ID of the expected rack

## Examples

```sh
nico-admin-cli expected-rack update 12345678-1234-5678-90ab-cdef01234567 --rack-profile-id abcdef01-2345-6789-abcd-ef0123456789
nico-admin-cli expected-rack update 12345678-1234-5678-90ab-cdef01234567 --rack-profile-id abcdef01-2345-6789-abcd-ef0123456789 --meta-name rack-01
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
