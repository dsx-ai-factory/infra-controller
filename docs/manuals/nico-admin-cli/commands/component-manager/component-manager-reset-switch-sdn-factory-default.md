# `nico-admin-cli component-manager reset-switch-sdn-factory-default`

*[Hardware commands](../../hardware.md) › [component-manager](./component-manager.md) › **reset-switch-sdn-factory-default***

## NAME

nico-admin-cli-component-manager-reset-switch-sdn-factory-default -
Reset switch SDN configuration directly through RMS (disrupts fabric
traffic)

## SYNOPSIS

```text
nico-admin-cli component-manager reset-switch-sdn-factory-default
<--switch-id> [--domain-name] [--extended]
[--sort-by] [-h|--help]
```

## DESCRIPTION

Reset SDN configuration for explicitly named switches through RMS.
Dispatch bypasses state-controller maintenance and disrupts fabric
traffic; it does not restore SDN configuration. Returns a parent job ID
without waiting for completion. Partial submission prints the job ID and
submission errors, then exits unsuccessfully. Do not automatically
resubmit after an RPC error because the reset may have started. Requires
RMS support for BatchResetSwitchSdnFactoryDefault.

## OPTIONS

`--switch-id <SWITCH_IDS>...`

Switch IDs to target

`--domain-name <DOMAIN_NAME>`

NVL certificate-domain selector; omit to use the RMS configured switch
domain

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
nico-admin-cli component-manager reset-switch-sdn-factory-default --switch-id sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
