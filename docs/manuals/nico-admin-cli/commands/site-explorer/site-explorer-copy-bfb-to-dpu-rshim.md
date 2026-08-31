# `nico-admin-cli site-explorer copy-bfb-to-dpu-rshim`

*[Tenant commands](../../tenant.md) › [site-explorer](./site-explorer.md) › **copy-bfb-to-dpu-rshim***

## NAME

nico-admin-cli-site-explorer-copy-bfb-to-dpu-rshim

## SYNOPSIS

**nico-admin-cli site-explorer copy-bfb-to-dpu-rshim** \[**--mac**\]
\<**--host-bmc-ip**\> \[**--pre-copy-powercycle**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*ADDRESS*\>

## DESCRIPTION

## OPTIONS

**--mac** *\<MAC\>*  
The MAC address the BMC sent DHCP from

**--host-bmc-ip** *\<HOST_BMC_IP\>*  
Host BMC IP address. Required for the mandatory post-copy host
power-cycle that applies the new BFB image to the DPU.

**--pre-copy-powercycle**  
Power-cycle the host before the BFB copy to release rshim control to the
DPU BMC.

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

\<*ADDRESS*\>  
BMC IP address or hostname with optional port

## Examples

```sh
nico-admin-cli site-explorer copy-bfb-to-dpu-rshim 192.0.2.10 --host-bmc-ip 192.0.2.20
nico-admin-cli site-explorer copy-bfb-to-dpu-rshim 192.0.2.10 --host-bmc-ip 192.0.2.20 --pre-copy-powercycle
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
