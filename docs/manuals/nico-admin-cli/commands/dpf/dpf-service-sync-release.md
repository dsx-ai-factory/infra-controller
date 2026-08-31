# `nico-admin-cli dpf service-sync release`

*[Hardware commands](../../hardware.md) › [dpf](./dpf.md) › [service-sync](./dpf-service-sync.md) › **release***

## NAME

nico-admin-cli-dpf-service-sync-release - Release the DPF maintenance
hold blocking a DPUService rollout

## SYNOPSIS

**nico-admin-cli dpf service-sync release** \[**--machine-id**\]
\[**--instance-id**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Release the DPF maintenance hold blocking a DPUService rollout

## OPTIONS

**--machine-id** *\<MACHINE_ID\>...*  
One or more host machine IDs to release

**--instance-id** *\<INSTANCE_ID\>...*  
Release the hosts running these instances, disrupting their tenants

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
nico-admin-cli dpf service-sync release --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80
nico-admin-cli dpf service-sync release --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
nico-admin-cli dpf service-sync release --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 --machine-id fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
nico-admin-cli dpf service-sync release --instance-id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli dpf service-sync release --instance-id 12345678-1234-5678-90ab-cdef01234567 abcdef01-2345-6789-abcd-ef0123456789
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
