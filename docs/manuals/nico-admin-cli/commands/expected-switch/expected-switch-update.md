# `nico-admin-cli expected-switch update`

*[Tenant commands](../../tenant.md) › [expected-switch](./expected-switch.md) › **update***

## NAME

nico-admin-cli-expected-switch-update - Update expected switch

## SYNOPSIS

```text
nico-admin-cli expected-switch update
[-a|--bmc-mac-address] [--id]
[-u|--bmc-username] [-p|--bmc-password]
[-s|--switch-serial-number] [--nvos-mac-address]
[--nvos-username] [--nvos-password] [--meta-name]
[--meta-description] [--label] [--rack_id]
[--bmc-ip-address] [--extended] [--nvos-ip-address]
[--bmc-retain-credentials] [--sort-by]
[-h|--help]
```

## DESCRIPTION

Update an expected switch.

Select the switch by either BMC MAC address or ID. Supply at least one
update field. Omitted fields and empty metadata names or descriptions
remain unchanged. Supplied labels replace the whole label collection.
Supplied NVOS MAC addresses replace the stored list.

Supply a username, password, or both for BMC or NVOS. Each omitted
credential field keeps its stored value. NVOS credentials must contain
both values after the update, so configuring them for the first time
requires both flags. Empty BMC values leave their fields unchanged; Core
PATCH rejects empty NVOS values. Legacy fallback uses the validation
rules on the older server.

The command first tries Core PATCH, which merges selected fields
atomically. It falls back to the legacy update on `Unimplemented` or
`PermissionDenied`, or when a MAC lookup returns no ID. Legacy
fallback preserves omitted fields and accepts either selector when Core
supports the switch update mask introduced in
[PR #3706](https://github.com/dsx-ai-factory/infra-controller/pull/3706). Earlier
servers use a full replacement: select by BMC MAC address and supply
every value you need to preserve. The legacy request still requires
authorization. Other PATCH errors and failed legacy updates remain
errors.

[Core PATCH RPCs](https://github.com/dsx-ai-factory/infra-controller/pull/6359)

## OPTIONS

`-a, --bmc-mac-address <BMC_MAC_ADDRESS>`

BMC MAC Address of the expected switch

`--id <ID>`

ID (UUID) of the expected switch to update.

`-u, --bmc-username <BMC_USERNAME>`

BMC username of the expected switch

`-p, --bmc-password <BMC_PASSWORD>`

BMC password of the expected switch

`-s, --switch-serial-number <SWITCH_SERIAL_NUMBER>`

Switch serial number of the expected switch

`--nvos-mac-address <NVOS_MAC_ADDRESSES>`

NVOS MAC address(es) of the expected switch

`--nvos-username <NVOS_USERNAME>`

NVOS username of the expected switch

`--nvos-password <NVOS_PASSWORD>`

NVOS password of the expected switch

`--meta-name <META_NAME>`

Replace the metadata name. An empty or omitted value leaves it unchanged

`--meta-description <META_DESCRIPTION>`

Replace the metadata description. An empty or omitted value leaves it
unchanged

`--label <LABEL>`

Replace all metadata labels with the supplied key or key:value entries.
Repeat for each label. Duplicate keys are rejected; label order is not
preserved. Omission preserves labels

`--rack_id <RACK_ID>`

Rack ID for this switch

`--bmc-ip-address <BMC_IP_ADDRESS>`

BMC IP address of the expected switch

`--extended`

Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

`--nvos-ip-address <NVOS_IP_ADDRESS>`

Static IP for the single wired NVOS port. The updated switch must have
exactly one NVOS MAC address. When Core supports PATCH or masked
updates, omit --nvos-mac-address only if the stored list already
contains exactly one MAC; otherwise, supply exactly one
--nvos-mac-address to replace the list. Older servers that support NVOS
IPs but replace the full record require exactly one --nvos-mac-address
in this command; omitted fields can be cleared. Servers without NVOS IP
support ignore this field

`--bmc-retain-credentials <BMC_RETAIN_CREDENTIALS>`

When true, site-explorer skips BMC password rotation and stores
factory-default credentials in Vault as-is

*Possible values:*

> - true
>
> - false

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
nico-admin-cli expected-switch update --bmc-mac-address 00:11:22:33:44:55 --bmc-username admin
nico-admin-cli expected-switch update --id 12345678-1234-5678-90ab-cdef01234567 --switch-serial-number DGX-H100-640GB
nico-admin-cli expected-switch update --bmc-mac-address 00:11:22:33:44:55 --nvos-password mynewpassword
nico-admin-cli expected-switch update --bmc-mac-address 00:11:22:33:44:55 --nvos-username admin --nvos-password mynewpassword
```

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
