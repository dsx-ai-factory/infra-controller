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

- **Admin CLI:** `nico-admin-cli nvl-domain health-report` lists and removes
  sources and prints report templates. It has no insertion command.
- **Admin HTTP:** Submit reports with the [HTTP procedure](#submit-a-report),
  including the first report for a domain.
- **Admin web UI:** NVLink domain health pages, linked from machine detail,
  display stored reports and provide a **Submit Health Report** form.

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

## Submit a report

Use `POST /admin/nvlink-domain/<DOMAIN_ID>/health/add-report` on the Admin web UI
endpoint. Until a report exists for the domain, the health page returns HTTP 404
and the **Submit Health Report** form is unavailable. The POST route accepts the
first report and makes the page and form available.

Before starting, obtain the NVLink domain UUID and access to your site's Admin
web UI over HTTPS. Configure the Admin CLI for the same site. The commands below
also require `curl` and `jq`. Use the `curl` authentication option that matches
your deployment's [Admin web UI authentication mode](debug_webui.md#authentication).
Add any TLS options your deployment requires.

- For `basic` (the default), use `--user admin` as shown below and enter the password
  when `curl` prompts. For a default Helm installation, use the command in the
  chart's release notes to retrieve the password from the
  `nico-api-web-basic-auth` Secret. If your deployment uses a custom credential,
  use its configured password.
- For `oauth2`, replace `--user admin` with `--cookie "$cookie_file"`, where
  `cookie_file` points to a curl cookie file containing the cookies for an active
  Admin web UI session on that endpoint. For automation, use the
  [client credentials flow](../playbooks/nico_web_oauth2.md#alternative-auth-flow)
  to obtain a session cookie.
- For `none`, omit `--user admin`. Supply any authentication your proxy requires.

Choose the apply mode before submitting. Mode values are case-sensitive:

| Submission mode | Effect |
|---|---|
| `Merge` | Adds this source to the domain's aggregate health. Resubmitting the same source replaces its previous report. |
| `Replace` | Makes this report the domain's aggregate health, masking all merge-mode reports. Only one replacement report is stored; another `Replace` submission replaces it, even with a different source. |

A `Replace` report with no alerts makes the aggregate report contain no alerts,
even if other sources report failures. Removing the replacement allows the
stored merge-mode reports to contribute again. Use `Merge` when adding health
information alongside other sources.

1. Set the Admin web UI origin, domain UUID, and your report source. Replace the
   example values with your site's values. Include the scheme and any port in
   `admin_url`, without a trailing slash or `/admin` path:

   ```sh
   admin_url='https://admin.example.com'
   domain_id='12345678-1234-5678-90ab-cdef01234567'
   report_source='overrides.sre-team'
   ```

1. Save a template and prepare your report. This example removes the template's
   sample probes and uses the server's submission time for `observed_at`:

   ```sh
   nico-admin-cli nvl-domain health-report print-empty-template \
     > nvlink-health-template.json
   jq --arg source "$report_source" \
     '.source = $source | .observed_at = null | .successes = [] | .alerts = []' \
     nvlink-health-template.json > nvlink-health-report.json
   ```

   Edit `nvlink-health-report.json` to add your actual successful probes and
   alerts. Use the [health report format](#report-format) for their fields.
   Keep `source` set to your reporting system or operator identity. Empty
   `successes` and `alerts` arrays submit a report with no probe results.

1. Wrap the report with the selected mode. The Admin HTTP endpoint requires
   `mode` and `health_report` at the top level:

   ```sh
   jq '{mode: "Merge", health_report: .}' \
     nvlink-health-report.json > nvlink-health-entry.json
   ```

   To submit a replacement, change `"Merge"` to `"Replace"` in this command.
   Review the resulting file before submission.

1. Submit the wrapped report directly to the domain's insertion route:

   ```sh
   curl --fail-with-body --silent --show-error \
     --user admin \
     --header 'Content-Type: application/json' \
     --data-binary @nvlink-health-entry.json \
     --write-out 'HTTP %{http_code}\n' \
     "$admin_url/admin/nvlink-domain/$domain_id/health/add-report"
   ```

   A successful submission returns HTTP 200 with an empty response body.
   An invalid domain UUID or an unsupported mode, such as `merge`, returns
   HTTP 400. Missing report fields or malformed JSON fail request validation.
   With Basic authentication, missing or invalid credentials return HTTP 401.
   With OAuth2, a missing or expired session redirects you to sign in.

1. Verify that your source and selected mode appear in the CLI output:

   ```sh
   nico-admin-cli nvl-domain health-report show "$domain_id"
   ```

   Open `/admin/nvlink-domain/<DOMAIN_ID>/health` on the same Admin web UI
   endpoint to inspect the report and its aggregate health. You can then use
   **Submit Health Report** for later submissions with the same JSON wrapper.
   To undo an operator report, [remove its source](#remove-a-report-source).

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
Removing the final source also removes the report storage for that domain.
The CLI then lists no entries, and the Admin health page returns HTTP 404 again.
Submit the next report with the [HTTP procedure](#submit-a-report).

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
schema as the rest of NICo health. The Admin HTTP insertion body wraps the
report as `{"mode": "Merge", "health_report": {...}}`. The CLI template contains
only the inner report; it needs this wrapper before HTTP submission. Refer to
[Health Report format](../architecture/health_aggregation.md#health-report-format)
for the field-by-field definition.
