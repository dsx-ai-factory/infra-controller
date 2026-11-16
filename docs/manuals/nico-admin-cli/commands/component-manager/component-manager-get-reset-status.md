# `nico-admin-cli component-manager get-reset-status`

*[Hardware commands](../../hardware.md) › [component-manager](./component-manager.md) › **get-reset-status***

## NAME

nico-admin-cli-component-manager-get-reset-status - Get reset status by
component and action

## SYNOPSIS

```text
nico-admin-cli component-manager get-reset-status [OPTIONS] --action <ACTION> <COMMAND>
```

## DESCRIPTION

Get reset status for each device and the required reset action. BMC
reset status is a successful no-op returning COMPLETED; it does not
verify recovery or require a prior submission. BMC factory reset is
unsupported. For SDN and NVOS resets on switches, NICo looks up its
latest successfully recorded reset reference for the selected action;
callers do not supply a job ID. After a failed or ambiguous submission,
this reference may still belong to an earlier reset. A missing reference
or unavailable reset record does not establish whether a reset ran.
Reset records can expire or become unavailable, even after completion
was previously observed. Failed resets and observation errors print
results and exit unsuccessfully.

## OPTIONS

`--action <ACTION>`

Reset action to inspect; bmc-reset status is a successful no-op

*Possible values:*

> - bmc-reset
>
> - bmc-factory-reset
>
> - nvos-factory-reset
>
> - sdn-factory-reset

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
nico-admin-cli component-manager get-reset-status --action bmc-reset switch --mac-address 00:11:22:33:44:55
nico-admin-cli component-manager get-reset-status --action sdn-factory-reset switch --switch-id sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60
nico-admin-cli -f json component-manager get-reset-status --action nvos-factory-reset switch --mac-address 00:11:22:33:44:55
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`switch`](./component-manager-get-reset-status-switch.md) | Get switch reset status by ID or BMC MAC |

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
