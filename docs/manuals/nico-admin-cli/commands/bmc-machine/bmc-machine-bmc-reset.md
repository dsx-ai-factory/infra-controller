# `nico-admin-cli bmc-machine bmc-reset`

*[Hardware commands](../../hardware.md) › [bmc-machine](./bmc-machine.md) › **bmc-reset***

## NAME

nico-admin-cli-bmc-machine-bmc-reset - Reset BMC

## SYNOPSIS

**nico-admin-cli bmc-machine bmc-reset** \[**--machine**\]
\[**--switch**\] \[**--power-shelf**\] \[**--reset-type**\]
\[**-u**\|**--use-ipmitool**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Reset a BMC.

Exactly one target must be specified: --machine, --switch, or
--power-shelf. Providing more than one target is rejected.

## OPTIONS

**--machine** *\<MACHINE\>*  
ID of the machine whose BMC to reset

**--switch** *\<SWITCH\>*  
ID of the switch whose BMC to reset

**--power-shelf** *\<POWER_SHELF\>*  
ID of the power shelf whose PMC to reset

**--reset-type** *\<RESET_TYPE\>*  
Redfish Manager.Reset type. Omit for the vendor default. Ignored with
--use-ipmitool.\

\
*Possible values:*

- graceful

- force

**-u**, **--use-ipmitool**  
Use ipmitool instead of Redfish to reset the BMC. ipmitool bmc reset
requests may be silently ignored if the BMC is in lockdown mode.

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
nico-admin-cli bmc-machine bmc-reset --machine fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
nico-admin-cli bmc-machine bmc-reset --switch sw100nt038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg --reset-type force
nico-admin-cli bmc-machine bmc-reset --power-shelf ps100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
nico-admin-cli bmc-machine bmc-reset --machine fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg --use-ipmitool
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
