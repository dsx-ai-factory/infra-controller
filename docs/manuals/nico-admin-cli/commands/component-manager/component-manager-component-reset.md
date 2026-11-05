# `nico-admin-cli component-manager component-reset`

*[Hardware commands](../../hardware.md) › [component-manager](./component-manager.md) › **component-reset***

## NAME

nico-admin-cli-component-manager-component-reset - Dispatch a component
reset directly (SDN reset disrupts fabric traffic)

## SYNOPSIS

```text
nico-admin-cli component-manager component-reset [OPTIONS] --action <ACTION> <COMMAND>
```

## DESCRIPTION

Dispatch a component reset directly, bypassing state-controller
maintenance. The RMS backend supports sdn-factory-reset and
nvos-factory-reset on switches. SDN reset disrupts fabric traffic and
restores SDN defaults without reinstalling NVOS. NVOS reset erases
operating-system configuration and reboots the switch; completion
requires SSH access with factory-default credentials. NVOS reset uses
the RMS configured default NVL certificate domain and rejects
--domain-name. Switches can be selected by ID or BMC MAC, including
expected inventory before ingestion. Success means accepted and tracked,
not completed. Per-device failures do not prevent other valid targets
from being submitted. A submission error may mean rejection or an
uncertain outcome; the reset may have started. Inspect the error and
device state before retrying. Do not automatically resubmit. Status
queries return the latest successfully recorded reset reference, which
may belong to an earlier reset after a failed or ambiguous submission.

## OPTIONS

`--action <ACTION>`

Reset action; the RMS backend supports sdn-factory-reset and
nvos-factory-reset

*Possible values:*

> - bmc-reset
>
> - bmc-factory-reset
>
> - nvos-factory-reset
>
> - sdn-factory-reset

`--domain-name <DOMAIN_NAME>`

NVL certificate-domain selector for SDN reset only; omit to use the RMS
configured domain

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
nico-admin-cli component-manager component-reset --action sdn-factory-reset switch --switch-id sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60
nico-admin-cli component-manager component-reset --action nvos-factory-reset switch --switch-id sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60
nico-admin-cli component-manager component-reset --action sdn-factory-reset --domain-name nvl-domain switch --mac-address 00:11:22:33:44:55
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`switch`](./component-manager-component-reset-switch.md) | Reset switches by ID or BMC MAC |

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
