# `nico-admin-cli power-shelf health-report add`

*[Hardware commands](../../hardware.md) › [power-shelf](./power-shelf.md) › [health-report](./power-shelf-health-report.md) › **add***

## NAME

nico-admin-cli-power-shelf-health-report-add - Insert a health report
source for a power shelf

## SYNOPSIS

**nico-admin-cli power-shelf health-report add** \[**--health-report**\]
\[**--template**\] \[**--message**\] \[**--replace**\]
\[**--print-only**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*POWER_SHELF_ID*\>

## DESCRIPTION

Insert a health report source for a power shelf

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
Replace all other health reports with this source

**--print-only**  
Print the template that is going to be send to carbide

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

\<*POWER_SHELF_ID*\>

## Examples

```sh
nico-admin-cli power-shelf health-report add 12345678-1234-5678-90ab-cdef01234567 --template internal-maintenance --message "Firmware upgrade in progress"
nico-admin-cli power-shelf health-report add 12345678-1234-5678-90ab-cdef01234567 --health-report '{...}'
nico-admin-cli power-shelf health-report add 12345678-1234-5678-90ab-cdef01234567 --template degraded --print-only
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
