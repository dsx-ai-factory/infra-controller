# `nico-admin-cli rms`

_[Hardware commands](../../hardware.md) › **rms**_

## NAME

nico-admin-cli-rms - RMS Actions

## SYNOPSIS

**nico-admin-cli rms** \[**--url**\] \[**--root-ca**\]
\[**--client-cert**\] \[**--client-key**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

RMS Actions

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

## Examples

```sh
nico-admin-cli rms --url https://rms.example.com:8443 inventory
nico-admin-cli rms power-on-sequence rack-1
nico-admin-cli rms --url https://rms.example.com:8443 --root-ca /etc/rms/ca.crt --client-cert /etc/rms/client.crt --client-key /etc/rms/client.key inventory
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`inventory`](./rms-inventory.md) | Get the full RMS inventory |
| [`power-on-sequence`](./rms-power-on-sequence.md) | Get the power on sequence |
| [`power-state`](./rms-power-state.md) | Get the power state for a given node |
| [`firmware-inventory`](./rms-firmware-inventory.md) | Get the firmware inventory for a given node |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
