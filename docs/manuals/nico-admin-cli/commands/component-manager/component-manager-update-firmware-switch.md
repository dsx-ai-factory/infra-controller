# `nico-admin-cli component-manager update-firmware switch`

*[Hardware commands](../../hardware.md) › [component-manager](./component-manager.md) › [update-firmware](./component-manager-update-firmware.md) › **switch***

## NAME

nico-admin-cli-component-manager-update-firmware-switch - Queue firmware
on NVLink switches

## SYNOPSIS

```text
nico-admin-cli component-manager update-firmware switch
[--bmc-username] [--bmc-password] [--nvos-username]
[--nvos-password] [--switch-id] [--mac-address]
[--target-version] [--sot-json-file] [--access-token]
[--force-update] [--component]
[--bypass-state-controller] [--extended] [--sort-by]
[-h|--help]
```

## DESCRIPTION

Queue firmware on NVLink switches

## OPTIONS

`--bmc-username <BMC_USERNAME>`

BMC username override for direct dispatch. Uses NICO_BMC_USERNAME when
the flag is omitted; requires a nonempty password via --bmc-password or
NICO_BMC_PASSWORD. Also used for power-shelf PMCs. Supplied credential
pairs apply to all directly dispatched targets and override Vault and
expected inventory. Omitted pairs use stored credentials; authentication
failures do not fall back. Requires a server supporting credential
overrides. Ignored for queued operations, rack firmware updates, and
standalone compute firmware reprovisioning; use
--bypass-state-controller for direct dispatch where supported. Core does
not save the overrides; NSM/PSM registration can persist them in Vault.
Subsequent status/version requests use their existing credential lookup.

`--bmc-password <BMC_PASSWORD>`

BMC password override; uses NICO_BMC_PASSWORD when omitted; requires a
BMC username

`--nvos-username <NVOS_USERNAME>`

Switch NVOS username override for direct dispatch. Uses
NICO_NVOS_USERNAME when the flag is omitted; requires a nonempty
password via --nvos-password or NICO_NVOS_PASSWORD. Ignored for compute
and power-shelf targets. Uses the same precedence, direct-dispatch
scope, and backend persistence behavior as --bmc-username.

`--nvos-password <NVOS_PASSWORD>`

Switch NVOS password override; uses NICO_NVOS_PASSWORD when omitted;
requires an NVOS username

`--switch-id <SWITCH_IDS>...`

Switch IDs to target

`--mac-address <MAC_ADDRESSES>...`

Device MAC addresses to target (BMC MAC for compute/switch, PMC MAC for
power shelf)

`--target-version <TARGET_VERSION>`

Firmware target version for legacy direct-update paths; exactly one of
--target-version and --sot-json-file is required

`--sot-json-file <PATH>`

SOT JSON file for RMS ApplyFirmwareObject; exactly one of
--target-version and --sot-json-file is required

`--access-token <ACCESS_TOKEN>`

Artifact access token for RMS SOT JSON downloads; omit or pass empty for
NOAUTH; only valid with --sot-json-file

`--force-update`

Force firmware update when supported

`--component <COMPONENTS>`

NVLink switch components to update; omit to update all supported
components

*Possible values:*

> - bmc
>
> - cpld
>
> - bios
>
> - nvos

`--bypass-state-controller`

Bypass the state controller and dispatch directly to the component
backend

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

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
