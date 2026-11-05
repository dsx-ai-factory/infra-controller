# `nico-admin-cli component-manager component-power-control`

*[Hardware commands](../../hardware.md) › [component-manager](./component-manager.md) › **component-power-control***

## NAME

nico-admin-cli-component-manager-component-power-control - Issue a
power-control action against components (switches, power shelves,
compute trays)

## SYNOPSIS

```text
nico-admin-cli component-manager component-power-control
[--bmc-username] [--bmc-password] [--nvos-username]
[--nvos-password] <--action>
[--bypass-state-controller] [--extended] [--sort-by]
[-h|--help] <subcommands>
```

## DESCRIPTION

Issue a power-control action against components (switches, power
shelves, compute trays)

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

`--action <ACTION>`

Power control action to apply to the targeted components

*Possible values:*

> - on
>
> - graceful-shutdown
>
> - force-off
>
> - graceful-restart
>
> - force-restart
>
> - ac-powercycle

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

Print help (see a summary with '-h')

## Examples

```sh
nico-admin-cli component-manager component-power-control --action on switch --switch-id sw100ntjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0
nico-admin-cli component-manager component-power-control --action on --bypass-state-controller --bmc-username admin --bmc-password mypassword --nvos-username admin --nvos-password mypassword switch --mac-address 00:11:22:33:44:55
nico-admin-cli component-manager component-power-control --action on switch --mac-address 00:11:22:33:44:55
nico-admin-cli component-manager component-power-control --action force-off compute-tray --machine-id fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
nico-admin-cli component-manager component-power-control --action force-off compute-tray --mac-address 00:11:22:33:44:55
nico-admin-cli component-manager component-power-control --action ac-powercycle power-shelf --power-shelf-id ps100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0
nico-admin-cli component-manager component-power-control --action ac-powercycle power-shelf --mac-address 00:11:22:33:44:55
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`switch`](./component-manager-component-power-control-switch.md) | Target NVLink switches |
| [`power-shelf`](./component-manager-component-power-control-power-shelf.md) | Target power shelves |
| [`compute-tray`](./component-manager-component-power-control-compute-tray.md) | Target compute trays |

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
