# `nico-admin-cli machine-validation plugins approve-full-host`

_[Hardware commands](../../hardware.md) › [machine-validation](./machine-validation.md) › [plugins](./machine-validation-plugins.md) › **approve-full-host**_

## NAME

nico-admin-cli-machine-validation-plugins-approve-full-host - Approve
full host access for a verified plugin revision

## SYNOPSIS

**nico-admin-cli machine-validation plugins approve-full-host**
\<**--test-id**\> \<**--version**\> \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Approve full host access for a verified plugin revision

## OPTIONS

**--test-id** *\<TEST_ID\>*  
**--version** *\<VERSION\>*  
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
nico-admin-cli machine-validation plugins verify --test-id gpu-health --version V1-T1720000000000000
nico-admin-cli machine-validation plugins approve-full-host --test-id gpu-health --version V1-T1720000000000000
nico-admin-cli machine-validation plugins enable --test-id gpu-health --version V1-T1720000000000000
nico-admin-cli machine-validation plugins disable --test-id gpu-health --version V1-T1720000000000000
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
