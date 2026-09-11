# `nico-admin-cli machine health-report add`

*[Hardware commands](../../hardware.md) › [machine](./machine.md) › [health-report](./machine-health-report.md) › **add***

## NAME

nico-admin-cli-machine-health-report-add - Insert a health report entry

## SYNOPSIS

**nico-admin-cli machine health-report add** \[**--health-report**\]
\[**--template**\] \[**--message**\] \[**--replace**\]
\[**--print-only**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*MACHINE_ID*\>

## DESCRIPTION

Insert a health report entry

## OPTIONS

**--health-report** *\<HEALTH_REPORT\>*  
New health report as json

**--template** *\<TEMPLATE\>*  
Predefined Template name. Use host-update for DPU Reprovision\

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

\<*MACHINE_ID*\>

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
