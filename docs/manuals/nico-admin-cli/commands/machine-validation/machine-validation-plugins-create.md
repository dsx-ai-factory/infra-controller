# `nico-admin-cli machine-validation plugins create`

*[Hardware commands](../../hardware.md) › [machine-validation](./machine-validation.md) › [plugins](./machine-validation-plugins.md) › **create***

## NAME

nico-admin-cli-machine-validation-plugins-create - Create an OCI Machine
Validation plugin

## SYNOPSIS

```text
nico-admin-cli machine-validation plugins create <--name>
[--type] [--container-plugin-adapter] <--image>
<--entrypoint> [--arg] [--parameters]
[--context] [--platform] [--timeout]
[--privileged] [--host-access-full] [--extended]
[--sort-by] [-h|--help]
```

## DESCRIPTION

Create an OCI Machine Validation plugin

## OPTIONS

`--name <NAME>`

`--type <PLUGIN_TYPE> [default: container]`

*Possible values:*

- container

`--container-plugin-adapter <CONTAINER_PLUGIN_ADAPTER>`

Container contract adapter; omit for the standard NICo contract

*Possible values:*

> - DSXHealthValidation

`--image <IMAGE>`

`--entrypoint <ENTRYPOINT>`

Command vector; repeat for arguments. Standard contract: appended to the
image ENTRYPOINT. DSXHealthValidation: first value overrides the
executable

`--arg <COMMAND_ARGS>`

Append a command argument; repeat as needed, using --arg=--flag for
flags

`--parameters <PARAMETERS> [default: {}]`

`--context <CONTEXT> [default: OnDemand]`

`--platform <PLATFORM>`

`--timeout <TIMEOUT> [default: 7200]`

`--privileged`

`--host-access-full`

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
nico-admin-cli machine-validation plugins create --name gpu-health --image registry.example.com/plugins/gpu-health@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --entrypoint /plugin/entrypoint --entrypoint check-gpus --context Discovery --platform HGX-B200 --parameters '{"expectedGpuCount":8}'
nico-admin-cli machine-validation plugins create --name host-gpu-health --image registry.example.com/plugins/host-gpu-health@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --entrypoint /plugin/entrypoint --context Discovery --platform HGX-B200 --parameters '{"expectedGpuCount":8}' --privileged --host-access-full
nico-admin-cli machine-validation plugins create --name dsx-health --type container --container-plugin-adapter DSXHealthValidation --image ghcr.io/dsx-ai-factory/health-validation@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --entrypoint /health-validation --arg run --context OnDemand --platform GB200 --parameters '{"gpu_count":4}'
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
