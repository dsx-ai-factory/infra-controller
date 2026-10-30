# `nico-admin-cli attestation spdm machines list`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [spdm](./attestation-spdm.md) › [machines](./attestation-spdm-machines.md) › **list***

## NAME

nico-admin-cli-attestation-spdm-machines-list - List machine attestation
statuses

## SYNOPSIS

```text
nico-admin-cli attestation spdm machines list [--machine-id]
[--redfish-timeout-secs] [--passed] [--failed]
[--cancelled] [--in-progress] [-h|--help]
```

## DESCRIPTION

List SPDM attestation statuses in machine-ID order.

Without --machine-id, uses every device in the latest retained
controller-triggered job for each machine, chosen by descending start
time and then descending job ID. External and older jobs are ignored;
BMCs are not contacted. Unfinished devices take precedence over
failures, cancellations, then all-passed. Without a status flag, lists
all machines; machines without a controller job show unknown (JSON/YAML:
null). Such machines are excluded by every status flag. Only one status
flag may be supplied. A partially collected job is in progress even if a
device has failed. The CLI fetches all pages automatically. Predictions
can change between pages. Prints the prediction warning once to stderr,
including when no machines match. With --machine-id, selects current
attesters using the resolved profile and BMC inventory, then aggregates
each selected devices latest result across all job origins. Missing
results mean in progress. This read-only assessment creates or changes
no jobs and prints no prediction warning. The table shows attester
selection status; JSON/YAML include the attester_selection_status field.
Empty or disabled selections have no verdict (table: not assessed;
JSON/YAML: null), with the reason in attester selection status. Partial
selection reports the selected subsets status and warns on stderr.
Machine lookup, BMC and timeout failures return errors. --machine-id
conflicts with all status flags. Supports table, JSON, and YAML output;
CSV is unsupported.

## OPTIONS

`--machine-id <MACHINE_ID>`

Definitively assess this machine using its current attesters

`--redfish-timeout-secs <REDFISH_TIMEOUT_SECS>`

Timeout in seconds for the entire single-machine assessment; defaults to
30; requires --machine-id

`--passed`

Only machines whose latest controller job passed for every device

`--failed`

Only machines whose latest controller job failed with no unfinished
devices

`--cancelled`

Only machines whose latest controller job has cancellations, with no
unfinished or failed devices

`--in-progress`

Only machines whose latest controller job has unfinished devices, even
if another device failed

`-h, --help`

Print help (see a summary with -h)

## Examples

```sh
nico-admin-cli attestation spdm machines list
nico-admin-cli attestation spdm machines list --passed
nico-admin-cli attestation spdm machines list --failed
nico-admin-cli attestation spdm machines list --cancelled
nico-admin-cli attestation spdm machines list --in-progress
nico-admin-cli attestation spdm machines list --machine-id fm100hsag07peffp850l14kvmhrqjf9h6jslilfahaknhvb6sq786c0g3jg --redfish-timeout-secs 60
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
