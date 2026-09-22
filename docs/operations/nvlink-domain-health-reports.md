# NVLink Domain Health Reports (Day 2)

Operator guide for managing **NVLink domain health reports**: health information
submitted by services, tools, or operators against an NVLink domain.

Each report is keyed by a **source** identifier. Multiple merge-mode sources
coexist and contribute to the domain's aggregate health. A replace-mode source
overrides that aggregate. Reports are persisted in the
`nvlink_domain_health_reports` table. See
[health report overrides](../architecture/health_aggregation.md#health-report-overrides)
for the same source-keyed model applied to hosts.

Management surfaces:

- **Admin CLI:** `nico-admin-cli nvl-domain health-report` (below).
- **Admin web UI:** NVLink domain health pages, linked from machine detail.

CLI RPC calls require the `Health` or `ForgeAdminCLI` RBAC permission
scope. Admin web UI access is controlled by its deployment authentication and
network policy.

Background: [Health Aggregation](../architecture/health_aggregation.md) and
[Monitoring and Health](./monitoring-health.md).

---

## Report sources

A **report source** is the identifier of the system or user that submitted a
report (the `HealthReport::source` field, for example,
`overrides.sre-team`). Keeping each reporting system or operator on a distinct
source lets their merge-mode reports coexist. Submitting a report with an
existing source replaces that source's prior report. `show` lists sources and
modes applied to a domain; `remove` deletes one source's report.

With `[collectors.nmxc]` and `[sinks.nvlink_domain_health_report]` enabled, the
hardware health service submits supported NMX-C controller health states under the
merge-mode source `hardware-health.nmxc-domain-state`. Both settings are
disabled by default. Collection starts only for primary switch-host endpoints
with NMX-C enabled in discovery metadata and an NVLink domain UUID.

The reported domain UUID must be valid and match the endpoint's NVLink domain
metadata. The NMX-C server header must report success. Notifications that fail
validation do not generate a domain health report. Configured log sinks still
receive the original NMX-C notification.

Controller health states map to reports as follows:

| NMX-C state | Report behavior |
|---|---|
| `Healthy` | Clears the `NmxControllerHealth` probe. |
| `Unhealthy` | Raises a `NmxControllerHealth` alert. |
| `UnhealthyDbCorrupted` | Raises a `NmxControllerHealth` alert. |
| `Degraded` or `Unknown` | Emits a log but does not generate a domain health report. |

NMX-C controller health alerts carry the `Hardware` classification and do not
add `PreventAllocations`.

Configuration validation rejects enabling
`[sinks.nvlink_domain_health_report]` with
`[collectors.nmxc.schema_override]`. The sink uses the configured NICo API
connection fields: `root_ca`, `client_cert`, `client_key`, and `api_url`.
Configuration is loaded when the hardware health service starts. Restart the
service to apply changes.

Stream loss does not remove the stored `hardware-health.nmxc-domain-state`
report. The latest report remains until a later `Healthy`, `Unhealthy`, or
`UnhealthyDbCorrupted` notification replaces it or an operator removes the
source. A later notification in one of these states can recreate a source that
an operator removed while collection remains active.

## CLI commands

### List report sources for a domain

```sh
nico-admin-cli nvl-domain health-report show <DOMAIN_ID>
```

Lists health report entries applied to the NVLink domain, including
source, mode, observation time, and alert count.

### Remove a report source

```sh
nico-admin-cli nvl-domain health-report remove <DOMAIN_ID> <REPORT_SOURCE>
```

Removes the report submitted under `<REPORT_SOURCE>` from the domain.

### Print an empty report template

```sh
nico-admin-cli nvl-domain health-report print-empty-template
```

Prints an empty health report template.

## Examples

```sh
# See which sources are reporting on a domain
nico-admin-cli nvl-domain health-report show 12345678-1234-5678-90ab-cdef01234567

# Clear a stale report submitted for maintenance
nico-admin-cli nvl-domain health-report remove 12345678-1234-5678-90ab-cdef01234567 internal-maintenance

# Get a template to build an insert payload
nico-admin-cli nvl-domain health-report print-empty-template
```

## Report format

NVLink domain health reports use the same `HealthReport` / `HealthProbeAlert`
schema as the rest of NICo health. Insertion wraps a report in a
`HealthReportEntry` that selects `Merge` or `Replace` mode. See
[Health Report format](../architecture/health_aggregation.md#health-report-format)
for the field-by-field definition.
