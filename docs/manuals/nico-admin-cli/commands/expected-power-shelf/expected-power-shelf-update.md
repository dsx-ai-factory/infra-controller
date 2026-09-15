# `nico-admin-cli expected-power-shelf update`

_[Tenant commands](../../tenant.md) › [expected-power-shelf](./expected-power-shelf.md) › **update**_

## NAME

nico-admin-cli-expected-power-shelf-update - Update expected power shelf

## SYNOPSIS

**nico-admin-cli expected-power-shelf update**
\[**-a**\|**--bmc-mac-address**\] \[**--id**\]
\[**-u**\|**--bmc-username**\] \[**-p**\|**--bmc-password**\]
\[**-s**\|**--shelf-serial-number**\] \[**--meta-name**\]
\[**--meta-description**\] \[**--label**\] \[**--host_name**\]
\[**--rack_id**\] \[**--bmc-ip-address**\]
\[**--bmc-retain-credentials**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Update expected power shelf

## OPTIONS

**-a**, **--bmc-mac-address** *\<BMC_MAC_ADDRESS\>*  
BMC MAC Address of the expected power shelf

**--id** *\<ID\>*  
ID (UUID) of the expected power shelf to update.

**-u**, **--bmc-username** *\<BMC_USERNAME\>*  
BMC username of the expected power shelf

**-p**, **--bmc-password** *\<BMC_PASSWORD\>*  
BMC password of the expected power shelf

**-s**, **--shelf-serial-number** *\<SHELF_SERIAL_NUMBER\>*  
Chassis serial number of the expected power shelf

**--meta-name** *\<META_NAME\>*  
The name that should be used as part of the Metadata for newly created
Power Shelves. If empty, the Power Shelf Id will be used

**--meta-description** *\<META_DESCRIPTION\>*  
The description that should be used as part of the Metadata for newly
created Power Shelves

**--label** *\<LABEL\>*  
A label that will be added as metadata for the newly created Machine.
The labels key and value must be separated by a : character

**--host_name** *\<HOST_NAME\>*  
Host name of the power shelf

**--rack_id** *\<RACK_ID\>*  
Rack ID for this power shelf

**--bmc-ip-address** *\<BMC_IP_ADDRESS\>*  
BMC IP address of the power shelf

**--bmc-retain-credentials** *\<BMC_RETAIN_CREDENTIALS\>*  
When true, site-explorer skips BMC password rotation and stores
factory-default credentials in Vault as-is\

\
*Possible values:*

- true

- false

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
nico-admin-cli expected-power-shelf update --bmc-mac-address 00:11:22:33:44:55 --bmc-username admin --bmc-password mynewpassword
nico-admin-cli expected-power-shelf update --id 12345678-1234-5678-90ab-cdef01234567 --shelf-serial-number DGX-H100-640GB
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
