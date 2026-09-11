# `nico-admin-cli expected-machine update`

*[Tenant commands](../../tenant.md) › [expected-machine](./expected-machine.md) › **update***

## NAME

nico-admin-cli-expected-machine-update - Update expected machine from
JSON file (full replacement, consistent with API).

## SYNOPSIS

**nico-admin-cli expected-machine update** \<**-f**\|**--filename**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Update expected machine from JSON file (full replacement, consistent
with API).

All fields from the JSON file will completely replace the existing
record. This allows clearing metadata fields by providing empty values.

Example json file: { "bmc_mac_address": "1a:1b:1c:1d:1e:1f",
"bmc_username": "user", "bmc_password": "pass", "chassis_serial_number":
"sample_serial-1", "fallback_dpu_serial_numbers": \["MT020100000003"\],
"metadata": { "name": "MyMachine", "description": "My Machine",
"labels": \[{"key": "ABC", "value": "DEF"}\] }, "sku_id": "sku_id_123" }

Usage: nico-admin-cli expected-machine update --filename machine.json

## OPTIONS

**-f**, **--filename** *\<FILENAME\>*  
Path to JSON file containing the expected machine data

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
nico-admin-cli expected-machine update --filename ./machine.json
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
