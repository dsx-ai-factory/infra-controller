# `nico-admin-cli machine hardware-info update gpus`

*[Hardware commands](../../hardware.md) › [machine](./machine.md) › [hardware-info](./machine-hardware-info.md) › [update](./machine-hardware-info-update.md) › **gpus***

## NAME

nico-admin-cli-machine-hardware-info-update-gpus - Update the GPUs of
this machine

## SYNOPSIS

**nico-admin-cli machine hardware-info update gpus** \<**--machine**\>
\<**--gpu-json-file**\> \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Update the GPUs of this machine

## OPTIONS

**--machine** *\<MACHINE\>*  
Machine ID of the server containing the GPUs

**--gpu-json-file** *\<GPU_JSON_FILE\>*  
JSON file containing GPU info. It should contain an array of JSON
objects like this: { "name": "string", "serial": "string",
"driver_version": "string", "vbios_version": "string",
"inforom_version": "string", "total_memory": "string", "frequency":
"string", "pci_bus_id": "string" } Pass an empty array if you want to
remove GPUs.

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

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
