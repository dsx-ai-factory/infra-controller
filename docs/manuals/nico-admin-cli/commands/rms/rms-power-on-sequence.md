# `nico-admin-cli rms power-on-sequence`

_[Hardware commands](../../hardware.md) › [rms](./rms.md) › **power-on-sequence**_

## NAME

nico-admin-cli-rms-power-on-sequence - Get the power on sequence

## SYNOPSIS

**nico-admin-cli rms power-on-sequence** \[**--url**\] \[**--root-ca**\]
\[**--client-cert**\] \[**--client-key**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*RACK_ID*\>

## DESCRIPTION

Get the power on sequence

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

## Examples

```sh
nico-admin-cli rms power-on-sequence rack-1
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
