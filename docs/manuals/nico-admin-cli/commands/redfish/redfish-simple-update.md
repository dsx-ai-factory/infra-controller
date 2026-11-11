# `nico-admin-cli redfish simple-update`

*[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **simple-update***

## NAME

nico-admin-cli-redfish-simple-update - Submit a SimpleUpdate
installation and return its task ID without waiting

## SYNOPSIS

```text
nico-admin-cli redfish --address <ADDRESS> simple-update [OPTIONS] --image-uri <IMAGE_URI> --target <TARGET>
```

## DESCRIPTION

Submit a SimpleUpdate installation and return its task ID without
waiting

Submit one Redfish SimpleUpdate request with TransferProtocol=HTTP. The
BMC retrieves the image; the CLI does not upload a local file. HTTP is
used for an image URI without a scheme; an explicit URI scheme takes
precedence. The image must be reachable from the BMC. Returns the task
ID without waiting for installation. Monitor progress with redfish
get-task --taskid TASK_ID.

## OPTIONS

`--image-uri <IMAGE_URI>`

Image URI for the BMC to retrieve

`--target <TARGET>`

Firmware inventory URI to update; repeat for multiple targets

`--extended`

Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

`--sort-by <SORT_BY> [default: primary-id]`

Sort output by specified field

*Possible values:*

> - primary-id: Sort by the primary ID
>
> - state: Sort by state

`-h, --help`

Print help (see a summary with -h)

## Examples

```sh
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword simple-update --image-uri carbide-pxe.forge//public/blobs/internal/aarch64/preingestion.bfb --target redfish/v1/UpdateService/FirmwareInventory/DPU_OS
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword simple-update --image-uri firmware.example.com/host-fw.bin --target /redfish/v1/UpdateService/FirmwareInventory/BIOS --target /redfish/v1/UpdateService/FirmwareInventory/BMC
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword get-task --taskid 0
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
