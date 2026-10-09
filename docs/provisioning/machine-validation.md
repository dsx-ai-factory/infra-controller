# Machine Validation <Badge intent="info">v2.0</Badge>

Machine Validation is NVIDIA Infra Controller's in-band validation framework for
checking a machine before it is made available to tenants. NICo uses Scout to run
validation tests on the host, collect the results, and report them back to the
site controller.

The framework is intended to be extensible. NICo provides a catalog of built-in
hardware validation tests. Site administrators add new site-specific container
validation through the plugin workflow described in
[Configuring Container Plugins](#configuring-container-plugins).

## Summary

Machine Validation helps operators answer a simple question: is this machine
healthy enough to enter or return to the tenant-ready pool?

NICo can run validation during lifecycle workflows such as discovery and release,
and administrators can also start validation on demand for a specific machine.
Each validation run selects tests based on context, platform support, test
enablement, verification state, tags, and any allow list supplied by the
operator.

In normal lifecycle validation, NICo runs only tests that are both enabled and
verified. Unverified tests can be exercised through on-demand validation before
they are promoted into the standard workflow.

## Audience

This guide is written for site administrators, SREs, platform administrators, and
developers who manage or extend NICo machine validation. The examples assume the
operator has access to the target site through `nico-admin-cli` and has the
permissions required to view or modify machine validation configuration.

## Prerequisites

Before using Machine Validation, confirm the following:

- Machine Validation is enabled for the site.
- The operator has privileges to view validation runs and manage validation
  tests.
- The target machine is under platform control and is not allocated to a tenant.
- Required validation images and tools are available for the selected tests.

## How Machine Validation Fits Into NICo

Machine Validation runs while a machine is under platform control and before it
is allocated to a tenant. Typical entry points include:

- Initial discovery, before a newly discovered machine reaches `Ready`.
- Cleanup or release workflows, before a returned machine is made available
  again.
- On-demand validation, when an administrator explicitly starts validation for a
  machine that needs additional checks.

Machine Validation complements SKU validation. SKU validation checks that the
machine inventory matches the expected hardware model. Machine Validation runs
tests on the machine to prove that the hardware and relevant host-side software
paths behave correctly.

## Framework Concepts

| Concept | Description |
| --- | --- |
| Validation run | One execution of Machine Validation for a machine. A run contains the selected tests and their results. |
| Test definition | The stored definition of a validation test, including command, arguments, image, contexts, supported platforms, timeout, tags, and version. |
| Context | The lifecycle situation in which a test is eligible to run. Common contexts are `Discovery`, `Cleanup`, and `OnDemand`. |
| Platform mapping | The list of machine platforms on which a test is supported. Scout uses the discovered machine platform to select compatible tests. |
| Enabled flag | Controls whether the test is eligible for selection. Disabled tests are not selected for normal validation. |
| Verified flag | Indicates that an administrator has validated the test itself. Normal lifecycle runs skip unverified tests. |
| Tags | Optional selectors that allow administrators to group tests and run targeted suites. |
| External config | A legacy named configuration file referenced by an existing catalog test. New OCI plugins use NICo registry credentials instead. |
| Result | The recorded output for one test execution, including status, timing, exit code, and captured output. |

## Test Selection

When a validation run starts, NICo and Scout select tests using the following
criteria:

1. The Machine Validation feature must be enabled for the site.
2. The test must be enabled, unless the site configuration explicitly overrides
   the catalog selection mode.
3. The test must be verified for normal lifecycle runs.
4. The test context must match the run context, such as `Discovery`, `Cleanup`,
   or `OnDemand`.
5. The test must support the machine platform.
6. If tags are supplied, the test must match the requested tags.
7. If an allow list is supplied, the test must be included in the allow list.

On-demand validation can intentionally include unverified tests with
`--run-unverified-tests`.

## Built-In Validation Coverage

The exact test IDs, versions, enabled state, and supported platforms are
deployment and release specific. Use `nico-admin-cli machine-validation tests
show` as the source of truth for the running site.

The built-in catalog commonly includes the following test groups:

| Area | Common tests | What they validate |
| --- | --- | --- |
| GPU health | `CudaSample`, `DcgmFullShort`, `DcgmFullLong` | CUDA execution, DCGM diagnostics, and basic GPU health. |
| GPU performance | `Nvbandwidth`, `RaytracingVk` | GPU memory bandwidth and graphics or compute paths used by supported platforms. |
| CPU | `CPUTestShort`, `CPUTestLong`, `CpuBenchmarkingFp`, `CpuBenchmarkingInt` | CPU stress and benchmark coverage for short and long validation windows. |
| Memory | `MemoryTestShort`, `MemoryTestLong`, `MmMemBandwidth`, `MmMemLatency`, `MmMemPeakBandwidth`, `MqStresserShort`, `MqStresserLong` | Memory stress, latency, bandwidth, and queue pressure. |
| Storage | `FioFile`, `FioPath`, `FioSSD` | File, path, and device-level I/O validation with fio-based tests. |
| Operational extensions | `DefaultTestCase`, runbook-style tests | Site or release-specific checks used to extend the validation workflow. |

Built-in tests delivered through NICo migrations are normally read-only. Legacy
test-definition mutation APIs are currently disabled. New site-specific
container validation is added through the plugin workflow.

## Site Configuration

Machine Validation is controlled by the site configuration. A minimal
configuration enables the feature:

```toml
[machine_validation_config]
enabled = true
```

A site can also control the catalog selection behavior:

```toml
[machine_validation_config]
enabled = true
test_selection_mode = "Default"
run_interval = "60s"
stale_run_timeout = "24h"
tests = [
  { id = "CudaSample", enable = true },
]
```

| Setting | Description |
| --- | --- |
| `enabled` | Enables or disables Machine Validation for the site. |
| `test_selection_mode` | Controls how configured tests are selected. `Default` uses the catalog and per-test settings, `EnableAll` enables all configured tests, and `DisableAll` disables all configured tests. |
| `run_interval` | Controls how often the controller processes pending validation work. |
| `stale_run_timeout` | Grace period before an active validation run is considered stale. The default is `24h`; configured values below `90s` are raised to `90s` so healthy runs are not failed between Scout heartbeats. |
| `tests` | Optional per-test overrides. Use the test identifiers reported by `tests show` for the running site. |

For container plugins, the site policy explicitly allows the supported plugin
type and image registry. An empty `allowed_plugin_types` list disables plugin
registration. Legacy tests are unaffected.

```toml
[machine_validation_config]
allowed_plugin_types = ["container"]
approved_plugin_registries = ["registry.example.com"]
allow_privileged_plugins = false
allow_full_host_plugins = false
```

Attempt logs default to enabled, with a 16 KiB chunk limit, a 1 MiB per-attempt
limit, and 30-day retention. Setting `enabled = false` makes NICo discard log
chunks. When enabled, both size limits must be greater than zero,
`max_chunk_bytes` must not exceed `max_attempt_bytes`, and the largest allowed
values are 16 KiB and 1 MiB respectively. `retention` accepts a non-negative
duration and controls when terminal-attempt logs are removed.

```toml
[machine_validation_config.attempt_logs]
enabled = true
max_chunk_bytes = 16384
max_attempt_bytes = 1048576
retention = "30d"
```

## Choose the Right Workflow

Use the workflow that matches the type of validation being managed:

| Need | Supported workflow |
| --- | --- |
| Run or inspect a built-in or existing legacy test | Use `machine-validation tests show`, and use the existing test's version to verify, enable, or disable it. |
| Add a new site-specific container validation | Use [Configuring Container Plugins](#configuring-container-plugins). Do not use `machine-validation tests add`. |
| Use NICo's baseline DCGM validation | Use [Official Basic Machine Validation Plugin](#official-basic-machine-validation-plugin). It is an optional container plugin. |
| Use a private registry for a container plugin | Use `nico-admin-cli credential registry set`; do not use legacy `container_auth`. |

Legacy definition mutations (`tests add`, `tests update`, and
`external-config add-update`) are disabled. The commands remain visible for
compatibility, but return `FAILED_PRECONDITION`.

### Legacy Catalog and Container Plugins

Both kinds of definition participate in the same Machine Validation selection,
run, result, verification, and enablement lifecycle. Their creation and runtime
contracts are different:

| Area | Existing legacy catalog | New container-plugin framework |
| --- | --- | --- |
| Source | Built-in or previously configured host and container test definitions. | A site-admin-created, immutable plugin revision. |
| Create or change | `tests add` and `tests update` are disabled. Existing definitions remain runnable. | `plugins create` creates a new digest-pinned revision. Changes require another revision. |
| Execution | Existing host commands or legacy container commands. | OCI container using the standard plugin input and result contract. |
| Registry credentials | Existing legacy container tests can use `container_auth`. | NICo credential manager through `credential registry set`. |
| Site controls | Existing Machine Validation enablement and per-test settings. | Plugin type, approved registries, and privileged or full-host policy in site configuration. |
| Lifecycle commands | `tests show` lists definitions; existing legacy definitions use `tests verify`, `tests enable`, and `tests disable`. | `tests show` also displays plugin revisions; use `plugins verify`, `plugins enable`, `plugins disable`, and, when needed, `plugins approve-full-host`. |

## Configuring Container Plugins

Container plugins are site-scoped Machine Validation test definitions. They use
the plugin catalog and standard plugin input/output contract; they are not
legacy container tests configured with `tests add`.

### 1. Allow the plugin runtime profile in site configuration

Add the policy to the site's existing Machine Validation configuration, then
roll out the normal site configuration deployment. With Helm, this is the TOML
in `nico-api.siteConfig.nicoApiSiteConfig`; with Kustomize, it is
`deploy/files/nico-api/nico-api-site-config.toml`. Keep the rest of the site
configuration intact.

```toml
[machine_validation_config]
enabled = true
allowed_plugin_types = ["container"]
approved_plugin_registries = ["registry.example.com"]
allow_privileged_plugins = false
allow_full_host_plugins = false
```

`approved_plugin_registries` contains registry hostnames, not complete image
paths. A plugin image must use one of these registries and an immutable digest.
Set `allow_privileged_plugins` or `allow_full_host_plugins` to `true` only when
the site has approved that runtime profile. Full-host access also requires a
separate per-revision approval.

### 2. Store a private-registry credential when needed

Public registries do not need a credential. For a private approved registry,
store the credential in NICo's credential manager. It is used only for the
image pull and is not placed in the plugin definition, input file, container,
or logs.

```sh
read -r -s -p 'Registry token: ' registry_token; printf '\n'
printf '%s' "$registry_token" | nico-admin-cli credential registry set \
  --registry registry.example.com \
  --username registry-user \
  --password-stdin
unset registry_token
```

The token is read from standard input, so it does not appear in the command
arguments or shell history. The registry hostname must exactly match the image
registry and an entry in `approved_plugin_registries`.

### 3. Create the plugin revision

Create a digest-pinned image definition. `--parameters` is optional non-secret
JSON that NICo supplies to the plugin through its standard input contract.
The command prints the new test ID and version; use those values in the
following steps.

```sh
nico-admin-cli machine-validation plugins create \
  --name gpu-health \
  --image registry.example.com/plugins/gpu-health@sha256:<image_digest> \
  --entrypoint /plugin/entrypoint \
  --entrypoint check-gpus \
  --context OnDemand \
  --context Discovery \
  --platform <platform> \
  --parameters '{"expectedGpuCount":8}'
```

Plugins run unprivileged by default. A privileged plugin requires
`--privileged` and `allow_privileged_plugins = true` in the site policy.
A plugin that also needs a writable host-root mount requires
`--host-access-full`, `allow_full_host_plugins = true`, and the approval in the
next step.

### 4. Qualify, verify, approve, and enable

First run a new plugin on demand on representative hardware. Include
`OnDemand` in its contexts for this qualification step. Include an additional
context such as `Discovery` when it should also be selected during the normal
lifecycle.

```sh
nico-admin-cli machine-validation on-demand start \
  --machine <machine_id> \
  --allowed-tests gpu-health \
  --run-unverified-tests
```

After reviewing the result and any attempt logs, verify and enable the exact
revision:

```sh
nico-admin-cli machine-validation plugins verify \
  --test-id gpu-health \
  --version <version>

nico-admin-cli machine-validation plugins enable \
  --test-id gpu-health \
  --version <version>
```

For a full-host plugin only, approve the verified revision before enabling it:

```sh
nico-admin-cli machine-validation plugins approve-full-host \
  --test-id gpu-health \
  --version <version>
```

Use `nico-admin-cli machine-validation tests show --test-id gpu-health` to
confirm the plugin type, image, verification, enablement, and full-host
approval state. Disable a revision when it must no longer be selected:

```sh
nico-admin-cli machine-validation plugins disable \
  --test-id gpu-health \
  --version <version>
```

## Official Basic Machine Validation Plugin

NICo provides an optional official basic Machine Validation plugin. It is a
small baseline plugin that uses the same container-plugin contract and
configuration workflow as a site-owned plugin. It is not registered or enabled
automatically.

The release pipeline publishes multi-architecture images named
`machine-validation-basic-plugin`. The current basic check is the host DCGM
diagnostic. NICo can add further baseline checks in future plugin releases
without changing the plugin contract. Sites can build and configure dedicated
plugins for detailed, long-running, hardware-specific, or workflow-specific
validation.

The basic plugin uses the DCGM installation on the target host. It requires the
privileged full-host profile, and the host must provide `dcgmi` at
`/usr/bin/dcgmi` by default. Set `dcgmiPath` when the host uses another absolute
path.

First allow the release registry and full-host profile in the site's plugin
policy. Add the published registry hostname while retaining any other approved
registries required by the site:

```toml
[machine_validation_config]
allowed_plugin_types = ["container"]
approved_plugin_registries = ["nvcr.io"]
allow_privileged_plugins = true
allow_full_host_plugins = true
```

Then register the exact released image digest. This example selects DCGM level
3; level 1 is also supported.

```sh
nico-admin-cli machine-validation plugins create \
  --name basic-machine-validation \
  --image 'nvcr.io/<registry-path>/machine-validation-basic-plugin@sha256:<digest>' \
  --entrypoint /nico-machine-validation-default-plugin \
  --context OnDemand \
  --context Discovery \
  --platform <platform> \
  --parameters '{"checks":[{"name":"dcgm-diagnostic","parameters":{"runLevel":3,"dcgmiPath":"/usr/bin/dcgmi"}}]}' \
  --privileged \
  --host-access-full
```

Qualify the revision on demand, then verify it, approve full-host access, and
enable it using the commands in
[Qualify, verify, approve, and enable](#4-qualify-verify-approve-and-enable).

Image digests are immutable. When NICo publishes an updated basic plugin, it
does not replace an enabled revision automatically. Register the new digest as
a new revision, qualify and approve it, then enable it. This preserves the
exact version used by every validation run.

## Legacy External Configuration

Existing catalog tests can reference named external configuration, including
the legacy `container_auth` configuration for legacy container commands. New
OCI plugins do not use this mechanism: use `credential registry set` in
[Configuring Container Plugins](#configuring-container-plugins) for a private
plugin registry.

The legacy `external-config add-update` API is currently disabled, so it cannot
be used to create or change external configuration. Existing configuration can
still be viewed or removed:

```sh
nico-admin-cli machine-validation external-config show --name container_auth
nico-admin-cli machine-validation external-config remove --name container_auth
```

## Viewing and Managing Existing Test Definitions

`machine-validation tests show` lists both legacy test definitions and plugin
revisions. The verify, enable, and disable commands in this section apply to
existing legacy definitions. For a plugin revision, use the
`machine-validation plugins` commands in
[Configuring Container Plugins](#configuring-container-plugins).

### List Tests

Use the test catalog to see the tests available in the site:

```sh
nico-admin-cli machine-validation tests show
```

Show a specific test:

```sh
nico-admin-cli machine-validation tests show --test-id <test_id>
```

Filter by platform or context:

```sh
nico-admin-cli machine-validation tests show --platforms <platform>
nico-admin-cli machine-validation tests show --contexts Discovery
```

Show unverified tests:

```sh
nico-admin-cli machine-validation tests show --show-un-verfied
```

The current CLI flag is spelled `--show-un-verfied`; use the spelling shown
above.

### Enable or Disable Existing Tests

Enable a test when it should be eligible for selection:

```sh
nico-admin-cli machine-validation tests enable \
  --test-id <test_id> \
  --version <version>
```

Disable a test when it should not be selected:

```sh
nico-admin-cli machine-validation tests disable \
  --test-id <test_id> \
  --version <version>
```

Use the `test_id` and `version` values returned by `tests show`. For a plugin
revision, use `machine-validation plugins enable` or
`machine-validation plugins disable` as shown above.

### Verify Existing Tests

Verify a test after it has been proven safe and correct for the target site:

```sh
nico-admin-cli machine-validation tests verify \
  --test-id <test_id> \
  --version <version>
```

Verification is a promotion step. For a new plugin revision, first qualify it
on demand, then use `machine-validation plugins verify` before enabling it for
normal lifecycle validation.

## Legacy Test Definitions

Existing host-command and legacy container-command definitions continue to run
unchanged. Their definition mutation APIs, including `machine-validation tests
add` and `machine-validation tests update`, are currently disabled and return
`FAILED_PRECONDITION`. Do not use them to add site-specific tests.

To add a new site-specific container validation, use the plugin lifecycle in
[Configuring Container Plugins](#configuring-container-plugins). It stores an
immutable, digest-pinned plugin revision and provides the required input/output
contract, registry credentials, verification, and enablement controls.

## Legacy Execution Models

Existing Machine Validation tests can be implemented as host commands or
legacy container-based commands. New container definitions use the plugin
workflow instead.

| Model | When to use it | Common fields |
| --- | --- | --- |
| Host command | Use when the test tool is already present in the discovery environment or host filesystem. | `--command`, `--args`, `--timeout` |
| Legacy container command | Used by existing catalog tests with a packaged dependency set or validation image. New definitions use `machine-validation plugins create`. | `--img-name`, `--container-arg`, `--external-config-file` |
| Host filesystem execution | Use when a containerized test must execute against the host filesystem. | `--execute-in-host true` |

Tests can also declare output file locations with `--extra-output-file` and
`--extra-err-file` when a command writes important diagnostics outside stdout or
stderr. Keep those outputs concise. Scout records command output for result
review, but Machine Validation is not a replacement for long-term log storage.

## Run Tracking and Stale Recovery

Machine Validation tracks active work at two levels:

- Run items show which tests were selected for a validation run.
- Attempts show each execution of a selected test, including state, timing, exit
  code, and output summaries.

Scout sends heartbeats while tests are running. The controller uses the latest
heartbeat to find work that stopped making progress. If a record has no
heartbeat, the controller falls back to the run start time, expected duration,
and `stale_run_timeout`.

When stale work is found, the controller fails the validation and records the
normal failed-validation health alert. This keeps a machine from staying stuck
in an active validation state after Scout stops reporting.

Operators can monitor this behavior with:

| Metric | Meaning |
| --- | --- |
| `carbide_machine_validation_oldest_active_age_seconds` | Age of the oldest active validation run. |
| `carbide_machine_validation_stale_runs_count` | Number of active validation runs considered stale in the latest reconciliation pass. |

## Database Changes and Upgrades

Recent Machine Validation reliability work introduced database schema changes
for execution tracking and stale recovery:

| Area | Database change |
| --- | --- |
| Execution tracking | Adds `machine_validation_run_items` and `machine_validation_attempts` tables. |
| Heartbeat recovery | Adds `machine_validation.last_heartbeat_at` and heartbeat indexes for active validations, run items, and attempts. |

Deployments must apply the normal API database migrations before relying on the
new run tracking and stale recovery behavior. No manual data backfill is
required for existing validation rows. Older rows without heartbeat timestamps
continue to use the duration-based stale detection fallback.

## Updating Plugin Revisions

Legacy `tests update` is currently disabled. Plugin revisions are immutable:
to change an image, entrypoint, parameters, timeout, or execution profile,
create a new plugin revision, qualify it on demand, verify it, and then enable
it. Disable the prior revision when it should no longer be selected.

## Extension Design Guidelines

Use the following guidelines when designing a new validation test:

| Area | Recommendation |
| --- | --- |
| Naming | Use a stable, descriptive PascalCase name such as `GpuFabricSmoke` or `StorageFioPath`. Avoid embedding temporary incident names or one-off ticket IDs. |
| Scope | Keep each test focused on one hardware or software concern. Prefer separate tests over a large script that hides multiple failure modes. |
| Contexts | Use `Discovery` for pre-allocation checks, `Cleanup` for checks after release, and `OnDemand` for operator-triggered validation or test qualification. |
| Platform support | Map tests only to platforms where the command, devices, firmware, and drivers are expected to exist. |
| Verification | Treat verification as a release gate for the test definition. Do not verify a test until it has passed on representative hardware. |
| Timeouts | Set an explicit timeout that matches the expected runtime. Long tests should be intentional and documented. |
| Secrets | Use NICo registry credentials for private plugin registries. Do not place secrets in plugin parameters, command arguments, input files, or output. |
| Output | Write concise stdout and stderr that explains what failed. Use extra output files only for diagnostics that cannot be emitted directly. |
| Pre-conditions | Use pre-conditions to skip tests that do not apply to a machine rather than failing unrelated platforms. |
| Tags | Add tags when operators need to run a targeted suite such as `gpu-smoke`, `storage`, or `burn-in`. |

## Running On-Demand Validation

Start validation for a specific machine:

```sh
nico-admin-cli machine-validation on-demand start --machine <machine_id>
```

Run only selected contexts:

```sh
nico-admin-cli machine-validation on-demand start \
  --machine <machine_id> \
  --contexts OnDemand
```

Run selected tests:

```sh
nico-admin-cli machine-validation on-demand start \
  --machine <machine_id> \
  --allowed-tests <test_id_1> \
  --allowed-tests <test_id_2>
```

Run a tagged suite:

```sh
nico-admin-cli machine-validation on-demand start \
  --machine <machine_id> \
  --tags gpu-smoke
```

Run unverified tests during qualification:

```sh
nico-admin-cli machine-validation on-demand start \
  --machine <machine_id> \
  --allowed-tests <test_id> \
  --run-unverified-tests
```

The `--run-unverified-tests` flag is intended for qualification only. Do not
use it for normal lifecycle validation.

## Viewing Runs and Results

Show validation runs:

```sh
nico-admin-cli machine-validation runs show
```

Show runs for one machine:

```sh
nico-admin-cli machine-validation runs show --machine <machine_id>
```

Include historical runs:

```sh
nico-admin-cli machine-validation runs show --machine <machine_id> --history
```

Show validation results for a machine:

```sh
nico-admin-cli machine-validation results show --machine <machine_id>
```

Show results for a specific validation run:

```sh
nico-admin-cli machine-validation results show --validation-id <validation_id>
```

Show a specific test result from a run:

```sh
nico-admin-cli machine-validation results show \
  --validation-id <validation_id> \
  --test-name <test_name>
```

## Viewing Plugin Attempt Logs

Container plugins can stream stdout and stderr to NICo while an attempt is
running. NICo stores this output only when attempt-log storage is enabled in the
site configuration. Built-in validation tests continue to expose their final
captured output through the existing results commands.

First, find the validation run for the machine, then list the attempts for the
plugin test:

```sh
nico-admin-cli machine-validation runs show --machine <machine_id>

nico-admin-cli machine-validation logs attempts \
  --validation-id <validation_id> \
  --test-id <test_id>
```

The attempt list includes an ID, attempt number, and state. Use an attempt ID to
inspect a completed or earlier retried attempt:

```sh
nico-admin-cli machine-validation logs show --attempt-id <attempt_id>
```

To view already stored output and continue polling while an active attempt
runs, use `follow`:

```sh
nico-admin-cli machine-validation logs follow --attempt-id <attempt_id>
```

The run-and-test selector resolves the current attempt, which is useful when an
operator does not yet have its ID:

```sh
nico-admin-cli machine-validation logs follow \
  --validation-id <validation_id> \
  --test-id <test_id>
```

By default, `show` and `follow` include a timestamp, stream, and sequence for
each output line. Use `--raw` for content only, or `--stdout-only` and
`--stderr-only` to select one stream:

```sh
nico-admin-cli machine-validation logs show \
  --attempt-id <attempt_id> \
  --stderr-only \
  --raw
```

Attempt logs are diagnostic data only. They do not determine the plugin result,
and a full buffer or the configured per-attempt limit can omit output. Plugin
authors must not write credentials, tokens, passwords, or private configuration
values to stdout or stderr because NICo stores emitted content without
redaction.

## Interpreting Results

Each test result records the command execution outcome, timing, exit code, and
captured output. A non-zero exit code indicates failure unless the test command
implements a documented skip or pre-condition behavior.

Scout captures bounded stdout and stderr after a test exits. Container plugins
also stream bounded diagnostic output while running when attempt-log storage is
enabled. Tests should print useful progress and final diagnostic information
without producing unbounded logs.

When a validation run fails, review:

- Whether the selected test is supported on the machine platform.
- Whether the test was verified and enabled intentionally.
- The command exit code and captured output.
- Any referenced external configuration.
- Whether the test timed out.
- Whether a pre-condition skipped or changed the intended execution path.

## Operational Guidance

Use Machine Validation as a controlled pre-allocation gate. Do not enable or
verify a new test in the standard lifecycle until it has been qualified with
on-demand runs on representative hardware.

For production sites:

- Keep the built-in catalog enabled according to the site's hardware and release
  policy.
- Use short tests for routine lifecycle validation and long tests for burn-in,
  repair validation, or targeted on-demand workflows.
- Prefer tags and allow lists for targeted validation instead of modifying the
  global catalog for temporary needs.
- Keep site-specific test names stable across releases so operators can compare
  historical results.
- Store private plugin-registry credentials with `credential registry set`.
- Review the output format of extension tests so failures are actionable from
  the CLI and admin UI.

## Troubleshooting

| Symptom | Common causes | Next step |
| --- | --- | --- |
| No tests are selected | Feature disabled, tests disabled, tests unverified, context mismatch, platform mismatch, tags do not match, or allow list excludes all tests. | Run `tests show` with the relevant platform and context, and confirm enabled and verified state. |
| A new plugin does not run in lifecycle validation | The plugin revision is unverified or disabled, or its context or platform does not match. | Run it on demand with `--run-unverified-tests`, review the result and logs, then verify and enable the revision. |
| I need to change a test definition | Legacy definition mutation is disabled; plugin revisions are immutable. | Create, qualify, verify, and enable a new plugin revision. |
| A test fails only on one platform | Platform mapping is too broad, platform-specific dependency is missing, or the test command assumes hardware that is not present. | Restrict `supported-platforms` or add a pre-condition. |
| A container plugin cannot start | The image, registry policy, or registry credential is incorrect. | Confirm the digest-pinned image, `approved_plugin_registries`, and NICo registry credential. |
| A test times out | Timeout is too short, the test is hung, or the machine is unhealthy. | Review captured output and set a deliberate timeout for the test's expected runtime. |
| Result output is incomplete | The test wrote too much output or logs outside captured stdout and stderr. | Keep CLI output concise and write important diagnostics before exit. |
