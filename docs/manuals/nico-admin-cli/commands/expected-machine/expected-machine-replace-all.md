# `nico-admin-cli expected-machine replace-all`

_[Tenant commands](../../tenant.md) › [expected-machine](./expected-machine.md) › **replace-all**_

## NAME

nico-admin-cli-expected-machine-replace-all - Replace all entries in the
expected machines table with the entries from an inputted json file.

## SYNOPSIS

**nico-admin-cli expected-machine replace-all**
\<**-f**\|**--filename**\> \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Replace all entries in the expected machines table with the entries from
an inputted json file.

Example json file: { "expected_machines": \[ { "bmc_mac_address":
"1a:1b:1c:1d:1e:1f", "bmc_username": "user", "bmc_password": "pass",
"chassis_serial_number": "sample_serial-1" }, { "bmc_mac_address":
"2a:2b:2c:2d:2e:2f", "bmc_username": "user", "bmc_password": "pass",
"chassis_serial_number": "sample_serial-2",
"fallback_dpu_serial_numbers": \["MT020100000003"\], "metadata": {
"name": "MyMachine", "description": "My Machine", "labels": \[{"key":
"ABC", "value": "DEF"}\] } } \] }

## OPTIONS

**-f**, **--filename** *\<FILENAME\>*  
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
nico-admin-cli expected-machine replace-all --filename ./expected-machines.json
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
