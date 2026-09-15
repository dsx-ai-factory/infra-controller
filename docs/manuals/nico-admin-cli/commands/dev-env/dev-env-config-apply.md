# `nico-admin-cli dev-env config apply`

_[Admin commands](../../admin.md) › [dev-env](./dev-env.md) › [config](./dev-env-config.md) › **apply**_

## NAME

nico-admin-cli-dev-env-config-apply - Apply devenv config

## SYNOPSIS

**nico-admin-cli dev-env config apply** \<**-m**\|**--mode**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\] \<*PATH*\>

## DESCRIPTION

Apply devenv config

## OPTIONS

**-m**, **--mode** *\<MODE\>*  
Vpc prefix, tenant network segment, or HostInband segment?\

\
*Possible values:*

- network-segment

- vpc-prefix

- host-inband-segment: Flat VPC plus HostInband segment for hosts with
  no DPU

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

\<*PATH*\>  
Path to devenv config file. Usually this is in forged repo at
envs/local-dev/site/site-controller/files/generated/devenv_config.toml

## Examples

```sh
nico-admin-cli dev-env config apply ./devenv_config.toml --mode network-segment
nico-admin-cli dev-env config apply ./devenv_config.toml --mode vpc-prefix
```

---

**See also:** [Admin commands](../../admin.md) · [CLI reference index](../../README.md)
