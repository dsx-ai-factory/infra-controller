# `nico-admin-cli rms firmware-inventory`

_[Hardware commands](../../hardware.md) › [rms](./rms.md) › **firmware-inventory**_

## NAME

nico-admin-cli-rms-firmware-inventory - Get the firmware inventory for a
given node

## SYNOPSIS

**nico-admin-cli rms firmware-inventory** \[**--url**\]
\[**--root-ca**\] \[**--client-cert**\] \[**--client-key**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*RACK_ID*\> \<*NODE_ID*\>

## DESCRIPTION

Get the firmware inventory for a given node

## OPTIONS

**--url** *\<URL\>*  
URL of RMS API endpoint (required).

**--root-ca** *\<ROOT_CA\>*  
Root CA path

**--client-cert** *\<CLIENT_CERT\>*  
Client certificate path

**--client-key** *\<CLIENT_KEY\>*  
Client key path

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
Rack ID to get power sequence for

\<*NODE_ID*\>  
Node ID to get firmware inventory for

## Examples

```sh
nico-admin-cli rms firmware-inventory rack-1 node-1
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
