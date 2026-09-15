# Pluggable Machine Validation Framework

## Software Design Document

## Revision History

| Version | Date | Modified By | Description |
| :---: | :---: | :---- | :---- |
| 0.1 | 2026-08-14 | Sunil Kumar | Initial draft |
|  |  |  |  |

# **1. Introduction**

Machine Validation checks whether a machine is ready to use. Today it runs a predefined set of validation tests. If a site needs a different check, it normally needs a Machine Validation code or catalog change, or it must run the check outside Machine Validation.

This design adds a supported plugin model. A site administrator can configure a plugin that contains a site-specific validation check. Machine Validation still owns the surrounding work: selecting the check, running it, enforcing its timeout, tracking its status, storing the result, and reporting the final machine outcome.

The first version supports plugins packaged as container images. It does not define the validation logic inside those containers.

## **1.1 Purpose**

The purpose of this document is to give NICo engineers, site administrators, and plugin authors a shared, simple design for site-owned Machine Validation checks. It focuses on:

1. Allowing an authorized site admin to configure a validation plugin without changing Machine Validation core code.
2. Giving every plugin the same input and output contract.
3. Running plugins safely and consistently through Scout.
4. Keeping Machine Validation responsible for lifecycle, status, timeout, and reporting behavior.

## **1.2 Definitions and Acronyms**

| Term/Acronym | Definition |
| :---- | :---- |
| NICo | NVIDIA bare-metal lifecycle management system. |
| SDD | Software Design Document. |
| Scout | Temporary host-side agent that runs during discovery and validation. |
| Machine Validation | Process that validates a machine before or after tenant use. |
| Plugin | A site-provided package that performs one validation check. |
| Plugin Definition | A site-scoped, versioned catalog entry that tells Machine Validation when and how to run a plugin. |
| Verified Revision | An immutable plugin revision that NICo has validated and the site admin has approved. Only verified revisions can be enabled. |
| Run | One Machine Validation request for one machine and context. |
| Run Item | One selected validation check inside a run. |
| Attempt | One execution of a run item. |
| OCI image | A standard container image format. |

## **1.3 Scope**

This SDD covers the framework needed to configure and run validation plugins.

1. Plugin configuration and validation.
2. A standard plugin input file and result file.
3. Container-based plugin execution in Scout.
4. Timeout, status, result, logging, audit, and visibility integration with existing Machine Validation behavior.
5. Safe defaults for permissions and package sources.

This SDD does not cover:

1. Creating or maintaining actual validation tests.
2. Plugin-specific business logic.
3. Every possible site-specific validation workflow.
4. Supporting scripts, archives, or remote services in the first release.
5. Replacing the existing Machine Validation lifecycle and orchestration.

### **1.3.1 Assumptions, Constraints, Dependencies**

Assumptions:

1. Scout is the execution environment for Machine Validation plugins.
2. Existing Machine Validation runs, results, and operator workflows continue to work.
3. A plugin can determine its own check result, but it cannot directly change Machine Validation run state.
4. Some verified plugin revisions may need root and direct hardware access to perform machine validation.

Constraints:

1. Plugins run unprivileged by default. A plugin may request privileged access only when site policy permits it; normal selection still requires verification.
2. A plugin must use a stable, versioned contract.
3. Each run must record the exact plugin version it used.
4. The first release runs selected plugins sequentially; future parallel execution must preserve the same input/output contract and run-item lifecycle.

Dependencies:

1. Existing durable run, run-item, and attempt tracking.
2. Scout container runtime and image-pull support.
3. Existing API, database, UI, CLI, and audit mechanisms for Machine Validation.

## **1.4 Requirements Summary**

### **1.4.1 Functional Requirements**

| ID | Requirement |
| :--- | :--- |
| FR-1 | Site admins can create plugin revisions, verify, approve full-host access, enable, disable, and view plugin definitions through the Machine Validation API and CLI. |
| FR-2 | A plugin definition states when the plugin applies, which container image to run, and its timeout and permission settings. |
| FR-3 | Machine Validation selects plugins using existing context, platform, and tag rules. |
| FR-4 | Scout gives every plugin a standard JSON input file. |
| FR-5 | Every plugin writes a versioned, schema-validated JSON result file with `pass`, `fail`, or `error`. |
| FR-6 | Machine Validation records plugin version, execution status, timeout, and result with the run item and attempt. |
| FR-7 | Existing run, result, API, and CLI workflows show plugin-backed results. |
| FR-8 | A plugin definition change does not affect a plugin that has already been selected for a run. |

### **1.4.2 Non-Functional Requirements**

| ID | Requirement |
| :--- | :--- |
| NFR-1 | Plugins run unprivileged by default. Privileged and full-host execution require explicit site policy; full-host execution also requires approval of the exact verified revision. |
| NFR-2 | Plugin packages use an immutable container image reference pinned to a digest. |
| NFR-3 | Plugin output, logs, and diagnostic files are size-limited before storage or display. |
| NFR-4 | Plugin configuration and execution are auditable. |
| NFR-5 | A plugin timeout, crash, or invalid result cannot leave the Machine Validation run stuck. |

# **2. System Architecture**

## **2.1 High-Level Architecture**

```text
Site admin manages a plugin in the Machine Validation catalog
        |
        v
Machine Validation selects the plugin for a machine
        |
        v
Scout downloads and starts the plugin container
        |
        v
Plugin reads its input file and writes its result file
        |
        v
Machine Validation stores the result and updates the run
```

Machine Validation remains the system of record. The plugin only performs its site-specific check. Scout runs the plugin and reports the outcome; the API owns the durable run state and the machine-controller continues to use that state as it does today.

**Configuration model:** site configuration sets the security guardrails; the Machine Validation plugin catalog manages individual plugins, their revisions, and their enabled state.

## **2.2 Component Breakdown**

| Component | Responsibility |
| :--- | :--- |
| Site admin | Creates, verifies, approves, enables, disables, and views site plugins through the Machine Validation catalog API and CLI. |
| Site configuration | Defines guardrails: approved registries and whether privileged or full-host plugins are permitted. |
| Machine Validation API | Stores immutable plugin revisions, applies site-config guardrails, selects plugins, records results, and owns run state. |
| Scout | Downloads, starts, monitors, and stops the plugin container. |
| Plugin | Performs one site-specific validation check and writes a result. |
| Admin CLI / API | Shows plugin configuration, execution details, and results. |

# **3. Detailed Design**

## **3.1 Plugin Definition**

A plugin definition is a site-scoped, immutable catalog revision. It contains
selection criteria, a digest-pinned OCI image, entrypoint, timeout, non-secret
parameters, and requested runtime profile. The Machine Validation API validates
the revision against site configuration guardrails, including approved registries
and permitted access. Scout receives only the selected, frozen revision.

Verification, enablement, and full-host approval are server-managed state, bound
to the exact revision and image digest. A new revision starts disabled and
unverified; it cannot set that state in its create or update request. Disabling
a definition stops future selection but does not change an already selected run.

### **3.1.1 Private Registry Credentials**

Site configuration permits registry hostnames; the NICo credential manager stores
credentials for private permitted registries. The plugin definition holds only
the digest-pinned image. Scout retrieves a matching credential just in time for
the image pull and never exposes it to the plugin, logs, or catalog. The legacy
`container_auth` configuration remains only for legacy tests.

## **3.2 Plugin Contract**

Every plugin receives a versioned, non-secret JSON input at
`/opt/forge/mv/input/input.json` by default and writes one versioned JSON result
to `/opt/forge/mv/output/result.json` by default. A Scout deployment can set its
common container-visible base directory with
`--machine-validation-plugin-contract-dir <absolute-path>`; Scout then mounts
`<base>/input` and `<base>/output` in every plugin container. The input identifies the run, attempt,
machine, context, plugin revision, deadline, and site-defined parameters. The
result declares `pass`, `fail`, or `error` with a short summary and optional
findings.

Scout writes the input file; the configured plugin entrypoint reads it. In many
images, a site-owned adapter is that entrypoint: it reads the framework input,
passes the site-defined parameters to the validation tool, and writes the
framework result. The run, attempt, machine, context, plugin, and deadline
fields are framework metadata that an adapter can use for log correlation or to
stop its own work before the deadline. The `parameters` object is the normal
place for values the validation itself uses, such as an expected GPU count.
Machine Validation does not interpret plugin-specific parameters.

### **3.2.1 Plugin Author Input Checklist**

A plugin author reads only the fields needed by that plugin:

1. Check that `contractVersion` is supported.
2. Read `parameters` for site-defined, non-secret validation settings.
3. Use `deadline` to stop work before the framework timeout where practical.
4. Optionally include `runId`, `runItemId`, and `attempt` in diagnostic logs.
5. Use `machineId`, `context`, and `plugin` only when the validation needs them.
6. Do not modify the input file, expect credentials in it, or treat its values as
   shell text. The plugin must write its final outcome to the result file; logs
   never determine the outcome.

The input and result each include a required `contractVersion`, initially `v1`.
NICo publishes JSON Schemas for that version with the API contract and validates
the result before accepting it. Compatible additions use the same version;
incompatible changes use a new version, while NICo continues to support earlier
versions for the documented compatibility period. A missing or invalid result,
timeout, image-pull failure, crash, or abnormal container exit is a framework
failure; it is not reported as a normal failed validation. The API contract also
defines size limits and adapter guidance.

## **3.3 Scout Execution Design**

Scout pulls the exact approved image within the attempt deadline, starts the
configured entrypoint without a shell, and manages the container lifecycle. The
default profile is non-root, network-disabled, capability-free, and
`no-new-privileges`; the input mount is read-only and the result mount is
writable.

| Execution mode | Container privileges | Writable host-root mount |
| :--- | :--- | :--- |
| Default | None | No |
| Privileged | Elevated container runtime privileges for approved hardware access | No |
| Full-host | Privileged | Yes, at `/host` |

Privileged plugins can access the approved hardware interfaces, but remain in
their own container filesystem. Full-host plugins can also read and modify the
host filesystem through `/host`.

Scout should use a streaming container runner. While the container is running,
it streams its stdout and stderr through the Machine Validation attempt-log
pipeline. Scout redacts sensitive values, applies chunk and retention limits,
and sends ordered log chunks associated with the run, run item, and attempt.
The result file remains separate: it is the structured final `pass`, `fail`, or
`error` outcome, while stdout and stderr provide live diagnostic detail.

Plugin authors write normal progress and diagnostics to stdout and warnings or
errors to stderr. They should flush output promptly because plugins do not run
with an interactive terminal, and must not print credentials, tokens, passwords,
or private configuration values. Plugins do not call Scout or use a logging SDK.
Scout may redact, chunk, truncate, and retain logs for only a limited time; logs
never determine the final validation outcome.

A verified revision may request privileged hardware access. Full-host access is
a separate, explicit request and requires separate approval for the exact
revision and image digest. It uses the legacy-style writable `/host` mount and
is appropriate only for trusted site code. A revision change requires new
verification and, when applicable, new full-host approval.

## **3.4 Result and Run Lifecycle**

Machine Validation selects enabled plugins using the existing context, platform,
and tag rules, snapshots the exact revision in the run item, and records the
attempt result. The plugin cannot update run state itself. Existing heartbeats,
reconciliation, retry policy, audit history, and status reporting apply unchanged.
A stale attempt is terminalized and cannot leave a machine in validation.

## **3.5 Future Parallel Execution**

The MVP executes plugins sequentially. The per-attempt input, output, identity,
and status model is independent, so it supports parallel scheduling without a
plugin contract change. When parallel execution is supported, `allowParallel`
and resource-class locking will control eligibility and protect shared hardware.

## **3.6 Admin UI and CLI Design**

Operators can see plugin configuration and execution details through the existing API and CLI. The framework reports why a plugin definition is rejected or excluded from selection. Additional UI, audit detail, and output-redaction work remains part of later milestones.

The plugin-aware CLI creates plugin definitions. The existing
`nico-admin-cli machine-validation tests add` command creates a legacy test
request and cannot create a plugin.

## **3.7 Compatibility and Migration**

Existing built-in validation tests continue to work unchanged through the legacy runner. They continue to use the existing command and result behavior, while sharing the existing run-item lifecycle, timeout handling, status tracking, and reporting.

New site plugins are created in the site-scoped plugin catalog and selected only for machines in that site. They are selected alongside built-in tests, but use the separate container-plugin runner. Only container plugins receive the standard input and result files.

Moving a built-in test to a separately packaged plugin is optional and can happen gradually. It is not required to introduce this framework.

## **3.8 Implementation Milestones**

| Delivery milestone | What is delivered |
| :--- | :--- |
| 1. MVP: Basic plugin execution | A site admin creates, verifies, and enables a digest-pinned container plugin through the CLI. Scout runs it sequentially, provides the standard input, reads the standard result, and records timeout, logs, status, and result. The MVP includes registry validation and plugin-owned adapters. |
| 2. Hardware and policy controls | Add `inputFiles`, resource limits, approved capabilities such as `gpu-read` and `host-journal-read`, and additional audit details. Privileged and separately approved full-host execution are already available through the initial policy controls. |
| 3. Production rollout | Show plugin details in operator tools, add metrics, emergency disable, and recovery handling for full-host access, test in site environments, then enable approved plugins for discovery validation. |

Parallel execution is not part of these delivery milestones. It can be added later when the framework supports it.

# **4. Technical Considerations**

## **4.1 Security and Observability**

The API accepts only digest-pinned images from approved registries and keeps
registry credentials out of plugin definitions, input, output, logs, and process
arguments. Verification, enablement, and full-host approval are server-managed
state bound to the exact revision and image digest. Plugin output is untrusted:
it is size-limited, redacted before persistence or display, access-controlled,
and recorded with the run, revision, and digest. Operators can distinguish a
validation `fail`, plugin `error`, and framework failure.

## **4.2 Acceptance Criteria**

1. A verified, enabled plugin is selected only for matching machines and contexts.
2. Scout executes its digest-pinned image using the approved runtime profile and
   records a terminal outcome.
3. Failed acquisition, timeout, crash, or invalid output cannot be accepted as a
   successful validation.
4. Existing non-plugin tests continue to work unchanged.

# **5. End-to-End Site Admin Example**

This example shows a privileged container with full-host access. Site
`example-ai-west-prod` needs a trusted Discovery check that requires root access
to GPU and low-level system interfaces and a writable host-root mount. The
existing health tool runs `health-check run --config operator.yaml`, so its
image includes a small adapter. The site team owns and supports that adapter and
its image. The adapter reads the framework input, runs the health tool, and
writes the framework result; Machine Validation never needs to understand the
health tool.

1. The site adds `registry.example.com` to the Machine Validation plugin policy
   in its normal `nico-api-site-config.toml` deployment configuration. The
   policy permits privileged and full-host plugins for the intended Discovery
   machines. Because the registry is private, the site configuration contains:

   ```toml
   [machine_validation_config]
   approved_plugin_registries = ["registry.example.com"]
   allow_privileged_plugins = true
   allow_full_host_plugins = true
   ```

   The site applies this through its normal configuration rollout, then stores
   its pull credential once. The hidden prompt and standard-input form keeps the
   token out of command arguments and shell history:

   ```sh
   read -r -s -p 'Registry token: ' registry_token; printf '\n'
   printf '%s' "$registry_token" | nico-admin-cli credential registry set \
     --registry registry.example.com \
     --username registry-user \
     --password-stdin
   unset registry_token
   ```

2. The site publishes the adapter image and records its immutable digest. The
   site admin creates this definition with the plugin-aware CLI:

   ```sh
   nico-admin-cli machine-validation plugins create \
     --name host-gpu-health \
     --context Discovery \
     --platform HGX-B200 \
     --image registry.example.com/example-ai-west-prod/host-gpu-health@sha256:<digest> \
     --entrypoint /plugin/entrypoint \
     --timeout 900 \
     --parameters '{"expectedGpuCount":8}' \
     --privileged \
     --host-access-full
   ```

   The API confirms the registry policy and creates an immutable disabled,
   unverified revision. The command prints its test ID and version, for example
   `Created plugin revision: forge_host_gpu_health 1.0.0`.

3. The site admin reviews the exact revision and image digest, verifies it, and
   grants the separate full-host approval before enabling it. On the next
   matching Discovery run, Machine Validation snapshots the revision and Scout
   pulls the image with the stored registry credential. The credential is never
   mounted into the container.

   For example, if the create response identifies test ID
   `forge_host_gpu_health` and version `1.0.0`, the site admin runs:

   ```sh
   nico-admin-cli machine-validation plugins verify \
     --test-id forge_host_gpu_health \
     --version 1.0.0
   nico-admin-cli machine-validation plugins approve-full-host \
     --test-id forge_host_gpu_health \
     --version 1.0.0
   nico-admin-cli machine-validation plugins enable \
     --test-id forge_host_gpu_health \
     --version 1.0.0
   ```

4. Scout writes `/opt/forge/mv/input/input.json`, starts the privileged
   `/plugin/entrypoint`, and provides the writable host root at `/host`. The
   adapter receives this illustrative input file inside its container:

   ```json
   {
     "contractVersion": "v1",
     "kind": "MachineValidationPluginInput",
     "runId": "a1b2c3d4-0000-0000-0000-000000000000",
     "runItemId": "e5f6a7b8-0000-0000-0000-000000000000",
     "attempt": 1,
     "machineId": "machine-1234",
     "context": "Discovery",
     "plugin": {
       "testId": "forge_host_gpu_health",
       "version": "1.0.0",
       "image": "registry.example.com/example-ai-west-prod/host-gpu-health@sha256:<digest>"
     },
     "deadline": "2026-08-25T12:15:00Z",
     "parameters": {
       "expectedGpuCount": 8
     }
   }
   ```

   `parameters` are site-defined, non-secret values. In this example,
   `expectedGpuCount` tells the adapter what the check should expect. The
   adapter runs:

   ```text
   health-check run --config operator.yaml
   ```

   If the tool finds eight healthy GPUs, the adapter writes this result file at
   `/opt/forge/mv/output/result.json`:

   ```json
   {
     "contractVersion": "v1",
     "kind": "MachineValidationPluginResult",
     "outcome": "pass",
     "summary": "Eight GPUs are healthy."
   }
   ```

5. Scout validates the result and Machine Validation records a successful run
   item. A tool failure maps to `fail`; an unreadable tool result, crash, pull
   failure, or timeout is recorded as a framework failure rather than a pass.
