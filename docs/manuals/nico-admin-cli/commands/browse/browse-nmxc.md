# `nico-admin-cli browse nmxc`

*[Hardware commands](../../hardware.md) › [browse](./browse.md) › **nmxc***

## NAME

nico-admin-cli-browse-nmxc - Run an NMX-C browse operation via the API
server

## SYNOPSIS

**nico-admin-cli browse nmxc** \[**--chassis-serial**\]
\[**--rack-id**\] \<**--operation**\> \[**--gpu-uid**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Run an NMX-C browse operation via the API server.

--operation is required. Exactly one endpoint selector must be
specified: --chassis-serial or --rack-id. Providing both selectors is
rejected.

## OPTIONS

**--chassis-serial** *\<CHASSIS_SERIAL\>*  
Chassis serial number (mutually exclusive with --rack-id)

**--rack-id** *\<RACK_ID\>*  
Rack ID; resolves the NMX-C endpoint from the racks ready control-plane
switch (mutually exclusive with --chassis-serial)

**--operation** *\<OPERATION\>*  
NMX-C browse operation to run\

\
*Possible values:*

- compute-node-info-list

- switch-node-info-list

- gpu-info

- gpu-info-list

- partition-info-list

- get-domain-properties

**--gpu-uid** *\<GPU_UID\>* \[default: 0\]  
GPU UID (used by the gpu-info operation)

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
nico-admin-cli browse nmxc --chassis-serial 1234567890 --operation gpu-info-list
nico-admin-cli browse nmxc --rack-id rack_vr_min_1 --operation gpu-info-list
nico-admin-cli browse nmxc --chassis-serial 1234567890 --operation compute-node-info-list
nico-admin-cli browse nmxc --chassis-serial 1234567890 --operation switch-node-info-list
nico-admin-cli browse nmxc --chassis-serial 1234567890 --operation gpu-info --gpu-uid 42
nico-admin-cli browse nmxc --chassis-serial 1234567890 --operation partition-info-list
nico-admin-cli browse nmxc --chassis-serial 1234567890 --operation get-domain-properties
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
