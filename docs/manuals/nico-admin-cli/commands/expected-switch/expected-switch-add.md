# `nico-admin-cli expected-switch add`

_[Tenant commands](../../tenant.md) › [expected-switch](./expected-switch.md) › **add**_

## NAME

nico-admin-cli-expected-switch-add - Add expected switch

## SYNOPSIS

**nico-admin-cli expected-switch add** \<**-a**\|**--bmc-mac-address**\>
\<**-u**\|**--bmc-username**\> \<**-p**\|**--bmc-password**\>
\<**-s**\|**--switch-serial-number**\> \[**--nvos-mac-address**\]
\[**--nvos-username**\] \[**--nvos-password**\] \[**--meta-name**\]
\[**--meta-description**\] \[**--label**\] \[**--rack_id**\]
\[**--bmc-ip-address**\] \[**--nvos-ip-address**\]
\[**--bmc-retain-credentials**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Add expected switch

## OPTIONS

**-a**, **--bmc-mac-address** *\<BMC_MAC_ADDRESS\>*  
BMC MAC Address of the expected switch

**-u**, **--bmc-username** *\<BMC_USERNAME\>*  
BMC username of the expected switch

**-p**, **--bmc-password** *\<BMC_PASSWORD\>*  
BMC password of the expected switch

**-s**, **--switch-serial-number** *\<SWITCH_SERIAL_NUMBER\>*  
Chassis serial number of the expected switch

**--nvos-mac-address** *\<NVOS_MAC_ADDRESSES\>*  
NVOS MAC address(es) of the expected switch

**--nvos-username** *\<NVOS_USERNAME\>*  
NVOS username of the expected switch

**--nvos-password** *\<NVOS_PASSWORD\>*  
NVOS password of the expected switch

**--meta-name** *\<META_NAME\>*  
The name that should be used as part of the Metadata for newly created
Switches. If empty, the SwitchId will be used

**--meta-description** *\<META_DESCRIPTION\>*  
The description that should be used as part of the Metadata for newly
created Machines

**--label** *\<LABEL\>*  
A label that will be added as metadata for the newly created Machine.
The labels key and value must be separated by a : character. E.g.
DATACENTER:XYZ

**--rack_id** *\<RACK_ID\>*  
Rack ID for this machine

**--bmc-ip-address** *\<BMC_IP_ADDRESS\>*  
BMC IP address of the expected switch

**--nvos-ip-address** *\<NVOS_IP_ADDRESS\>*  
Static IP for the single wired NVOS port. Requires exactly one
--nvos-mac-address

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
nico-admin-cli expected-switch add --bmc-mac-address 00:11:22:33:44:55 --bmc-username admin --bmc-password mypassword --switch-serial-number DGX-H100-640GB
nico-admin-cli expected-switch add --bmc-mac-address 00:11:22:33:44:55 --bmc-username admin --bmc-password mypassword --switch-serial-number DGX-H100-640GB --rack_id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli expected-switch add --bmc-mac-address 00:11:22:33:44:55 --bmc-username admin --bmc-password mypassword --switch-serial-number DGX-H100-640GB --nvos-mac-address aa:bb:cc:dd:ee:ff --nvos-username admin --nvos-password mypassword --nvos-ip-address 192.0.2.10
nico-admin-cli expected-switch add --bmc-mac-address 00:11:22:33:44:55 --bmc-username admin --bmc-password mypassword --switch-serial-number DGX-H100-640GB --meta-name spine-01 --label DATACENTER:XYZ
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
