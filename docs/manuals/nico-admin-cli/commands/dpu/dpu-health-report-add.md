# `nico-admin-cli dpu health-report add`

_[Hardware commands](../../hardware.md) › [dpu](./dpu.md) › [health-report](./dpu-health-report.md) › **add**_

## NAME

nico-admin-cli-dpu-health-report-add - Insert a health report source for
a DPU

## SYNOPSIS

**nico-admin-cli dpu health-report add** \[**--health-report**\]
\[**--template**\] \[**--message**\] \[**--replace**\]
\[**--print-only**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*DPU_ID*\>

## DESCRIPTION

Insert a health report source for a DPU

## OPTIONS

**--health-report** *\<HEALTH_REPORT\>*  
New health report as json

**--template** *\<TEMPLATE\>*  
Predefined Template name\

\
*Possible values:*

- host-update

- internal-maintenance

- out-for-repair

- degraded

- validation

- suppress-external-alerting

- mark-healthy

- stop-reboot-for-automatic-recovery-from-state-machine

- tenant-reported-issue

- request-online-repair

- request-repair

**--message** *\<MESSAGE\>*  
Message to be filled in template.

**--replace**  
Replace the DPU health contribution with this source

**--print-only**  
Print the template that is going to be sent to carbide

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

\<*DPU_ID*\>

## Examples

```sh
nico-admin-cli dpu health-report add 12345678-1234-5678-90ab-cdef01234567 --template internal-maintenance
nico-admin-cli dpu health-report add 12345678-1234-5678-90ab-cdef01234567 --template out-for-repair --message "awaiting replacement part"
nico-admin-cli dpu health-report add 12345678-1234-5678-90ab-cdef01234567 --health-report '{"status":"Degraded"}'
nico-admin-cli dpu health-report add 12345678-1234-5678-90ab-cdef01234567 --template internal-maintenance --replace
nico-admin-cli dpu health-report add 12345678-1234-5678-90ab-cdef01234567 --template internal-maintenance --print-only
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
