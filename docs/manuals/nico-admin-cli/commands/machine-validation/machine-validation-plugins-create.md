# `nico-admin-cli machine-validation plugins create`

_[Hardware commands](../../hardware.md) › [machine-validation](./machine-validation.md) › [plugins](./machine-validation-plugins.md) › **create**_

## NAME

nico-admin-cli-machine-validation-plugins-create - Create an OCI Machine
Validation plugin

## SYNOPSIS

**nico-admin-cli machine-validation plugins create** \<**--name**\>
\<**--image**\> \<**--entrypoint**\> \[**--parameters**\]
\[**--context**\] \[**--platform**\] \[**--timeout**\]
\[**--privileged**\] \[**--host-access-full**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Create an OCI Machine Validation plugin

## OPTIONS

**--name** *\<NAME\>*  
**--image** *\<IMAGE\>*  
**--entrypoint** *\<ENTRYPOINT\>*  
**--parameters** *\<PARAMETERS\>* \[default: {}\]  
**--context** *\<CONTEXT\>* \[default: OnDemand\]  
**--platform** *\<PLATFORM\>*  
**--timeout** *\<TIMEOUT\>* \[default: 7200\]  
**--privileged**  
**--host-access-full**  
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

## Examples

```sh
nico-admin-cli machine-validation plugins create --name gpu-health --image registry.example.com/plugins/gpu-health@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --entrypoint /plugin/entrypoint --entrypoint check-gpus --context Discovery --platform HGX-B200 --parameters '{"expectedGpuCount":8}'
nico-admin-cli machine-validation plugins create --name host-gpu-health --image registry.example.com/plugins/host-gpu-health@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --entrypoint /plugin/entrypoint --context Discovery --platform HGX-B200 --parameters '{"expectedGpuCount":8}' --privileged --host-access-full
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
