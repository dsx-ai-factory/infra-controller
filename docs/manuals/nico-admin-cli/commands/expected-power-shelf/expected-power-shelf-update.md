# `nico-admin-cli expected-power-shelf update`

*[Tenant commands](../../tenant.md) › [expected-power-shelf](./expected-power-shelf.md) › **update***

## NAME

nico-admin-cli-expected-power-shelf-update - Update expected power shelf

## SYNOPSIS

```text
nico-admin-cli expected-power-shelf update
[-a|--bmc-mac-address] [--id]
[-u|--bmc-username] [-p|--bmc-password]
[-s|--shelf-serial-number] [--meta-name]
[--meta-description] [--label] [--host_name]
[--rack_id] [--bmc-ip-address]
[--bmc-retain-credentials] [--extended] [--sort-by]
[-h|--help]
```

## DESCRIPTION

Update an expected power shelf.

Select the shelf by either BMC MAC address or ID. Supply BMC credentials
or a shelf serial number; other update flags must accompany one of those
options. With Core PATCH, supplied fields replace their stored values
and omitted fields remain unchanged. Supplied labels replace the whole
label collection. An empty metadata name or description clears that
field.

Supply BMC username and password together. Core PATCH requires both
values to be nonempty; omitting both preserves credentials. Legacy
fallback uses the validation rules on the older server.

The command first tries Core PATCH, which merges selected fields
atomically. It falls back to the legacy update on `Unimplemented` or
`PermissionDenied`, or when a MAC lookup returns no ID. The legacy
shelf update sends a full replacement: omitted values can be cleared,
and selecting by ID is rejected. For this path, select by BMC MAC
address and supply every value you need to preserve. The legacy request
still requires authorization. Other PATCH errors and failed legacy
updates remain errors.

[Core PATCH RPCs](https://github.com/dsx-ai-factory/infra-controller/pull/6359)

## OPTIONS

`-a, --bmc-mac-address <BMC_MAC_ADDRESS>`

BMC MAC Address of the expected power shelf

`--id <ID>`

ID (UUID) of the expected power shelf to update.

`-u, --bmc-username <BMC_USERNAME>`

BMC username of the expected power shelf

`-p, --bmc-password <BMC_PASSWORD>`

BMC password of the expected power shelf

`-s, --shelf-serial-number <SHELF_SERIAL_NUMBER>`

Chassis serial number of the expected power shelf

`--meta-name <META_NAME>`

Replace the metadata name. An empty value clears it; PATCH preserves it
when omitted

`--meta-description <META_DESCRIPTION>`

Replace the metadata description. An empty value clears it; PATCH
preserves it when omitted

`--label <LABEL>`

Replace all metadata labels with the supplied key or key:value entries.
Repeat for each label. PATCH preserves omitted labels

`--host_name <HOST_NAME>`

Unsupported for expected power shelf updates. Omit this option

`--rack_id <RACK_ID>`

Rack ID for this power shelf

`--bmc-ip-address <BMC_IP_ADDRESS>`

BMC IP address of the power shelf

`--bmc-retain-credentials <BMC_RETAIN_CREDENTIALS>`

When true, site-explorer skips BMC password rotation and stores
factory-default credentials in Vault as-is

*Possible values:*

> - true
>
> - false

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
nico-admin-cli expected-power-shelf update --bmc-mac-address 00:11:22:33:44:55 --bmc-username admin --bmc-password mynewpassword
nico-admin-cli expected-power-shelf update --id 12345678-1234-5678-90ab-cdef01234567 --shelf-serial-number DGX-H100-640GB
```

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
