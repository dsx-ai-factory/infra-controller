# `nico-admin-cli redfish set-bios`

*[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **set-bios***

## NAME

nico-admin-cli-redfish-set-bios - Set BIOS options

## SYNOPSIS

**nico-admin-cli redfish set-bios** \<**--attributes**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Set BIOS options

## OPTIONS

**--attributes** *\<ATTRIBUTES\>*  
BIOS attributes to set in JSON, ex:
{"OperatingModes_ChooseOperatingMode": "MaximumPerformance"}

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
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword set-bios --attributes '{"OperatingModes_ChooseOperatingMode": "MaximumPerformance"}'
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
