# NICo's Machine Validation Framework - Declarative Approach

## Software Design Proposal

## Revision History

| Version | Date | Modified By | Description |
| :---: | :---: | :--- | :--- |
| 0.1 | 2026-10-13 | Sunil Kumar | Proposal for declarative plugin invocation and result interpretation |
| 0.2 | 2026-10-15 | Sunil Kumar | Add the OSS-dependent health-validator integration option |
| 0.3 | 2026-10-15 | Sunil Kumar | Center the proposal on the declarative approach and move health-validator adoption into appendices |

## 1. Context and Proposal

NICo's Machine Validation framework runs plugin containers on a machine through Scout and records their results. This proposal extends that framework so a validator can keep its own command, configuration, and report format.

### 1.1 What NICo Already Supports

| Area | Existing support |
| --- | --- |
| Registration | Container image, entrypoint arguments, JSON parameters, timeout, contexts, and platforms |
| Lifecycle and access | Versioned catalog entries, verification, enable/disable controls, and site-approved runtime access; full-host access requires explicit revision approval |
| Input and execution | Scout writes `input/input.json`, mounts inputs read-only, runs the container, enforces the timeout, and captures bounded logs |
| Result | After process exit 0, Scout reads `output/result.json`: outcome `pass`, `fail`, or `error`, summary, optional severity, and findings |

This is the native `v1` contract. A plugin can complete successfully and report `fail` because the machine is unhealthy. A nonzero process exit is an execution error. Warnings are expressed as severity, not a separate native outcome. Appendix A gives an example.

The implementation is in the [plugin CLI](../../crates/admin-cli/src/machine_validation/plugins.rs), [Scout runner](../../crates/machine-validation/src/plugin_runner.rs), and [result contract](../../crates/machine-validation/src/plugin_contract.rs).

### 1.2 The Integration Gap

Existing validators may use their own configuration files, stdout reports, and exit codes. They need translation to NICo's contract. For example, health-validation emits a JSON report on stdout and can exit 1 after reporting an unhealthy machine. The native runner expects a result file and exit 0.

### 1.3 Proposed Direction

Use **declarative plugin configuration** to describe a tool's invocation and result meanings as data. NICo generates the inputs, runs the container, interprets the report, and applies site policy. Providers maintain YAML definitions instead of writing a NICo-specific adapter for each supported contract.

Build this capability when an integration needs it. The immediate OSS health-validation integration can instead use a small internal adapter selected through a new plugin type.

| Route | Who translates the tool's contract? | Example |
| --- | --- | --- |
| Existing native plugin | The plugin itself or its wrapper | Reads `input.json`, writes `result.json`; Appendix A |
| Interim OSS integration | NICo code for a known plugin type | Proposed `--type DSXMachineHealthValidation`; Appendix B |
| Declarative onboarding | NICo uses provider-supplied configuration | Commands, native inputs, and result mappings in YAML; Appendix C |

**Status:** The native contract exists. The DSX plugin type, declarative fields, file-import options, and update operation described below are proposals, not implemented interfaces.

## 2. Declarative Configuration

A plugin definition describes the command, optional input files, and result rules. A site-approved runtime profile supplies environment variables, mounts, and permissions.

| Owner | Responsibility |
| --- | --- |
| Provider | Validation logic, command, report schema, and exit-code meanings |
| Site administrator | Hardware expectations, required checks, runtime approval, and warning policy |
| Scout | Resolve inputs and host dependencies, run the container, and capture output and exit status |
| Machine Validation API | Validate the result, apply site policy, and record the final outcome |

For each attempt, NICo selects an approved revision, Scout prepares and executes the invocation, and NICo interprets the captured result. The selected revision pins the image digest, runtime profile, and interpretation rules.

### 2.1 Where Configuration Lives

- **Provider repository:** released plugin definitions and report schemas. This repository may be private.
- **Site configuration repository:** site values and approved runtime profiles, combined with the provider definition for import.
- **NICo catalog:** the approved immutable revision used at runtime. Scout receives this snapshot from NICo.

Scout generates files such as `operator.yaml` per attempt. Site-level containerd namespace and snapshotter settings are provisioned through Scout deployment configuration. Scout does not fetch the provider repository during a run.

### 2.2 Adding and Updating a Plugin

Extend **nico-admin-cli**, backed by the Machine Validation API, to import YAML and schemas. A proposed registration command is:

```text
nico-admin-cli machine-validation plugins create \
  --config-file plugin.yaml \
  --runtime-config scout-runtime.yaml \
  --schema-file provider-report-v1=provider-report.schema.json \
  --context Discovery \
  --platform <site-platform>
```

The new import options submit the definition, schema, and selected runtime profile. The API validates them and returns a test ID and disabled, unverified revision. The administrator reviews, verifies, approves runtime access as required, and enables that revision.

For a change, edit the source YAML and use a proposed `plugins update --test-id <test-id>` operation with the same import options. This creates a new revision for review and enablement. Editing YAML alone has no runtime effect; existing attempts keep their selected revision. Rollback selects a previously approved revision.

Create, verify, enable, disable, and full-host approval already exist for native plugins. File import and revision-creating update are additions proposed here.

## 3. Result Interpretation

The result definition states both **where the assessment comes from** and **what the process exit means**:

| Result format | Interpretation |
| --- | --- |
| Existing NICo contract | Preserve the native result file and execution rules |
| JSON or YAML report | Read stdout or a declared file, validate its schema, and map a selected field |
| Exit code only | Map documented exit codes directly to an assessment or execution error |

For an exit-code-only tool, an illustrative mapping is:

```yaml
result:
  format: exit-code
  exitCodes:
    0: pass
    3: warn
    1: fail
    2: error
```

For structured reports, declare which exit codes permit report interpretation. An accepted exit does not itself mean the machine passed. Undeclared exits, missing or invalid reports, and unrecognized values never become pass.

| Assessment or execution state | Final NICo outcome |
| --- | --- |
| Pass with required checks assessed | pass |
| Warning | pass with warning severity, or fail if site policy blocks warnings |
| Fail or unknown assessment | fail |
| Required checks not assessed | fail with incomplete coverage |
| Execution failure, timeout, or invalid result | error |

Preserve the provider assessment alongside the final policy decision. A warning blocked by site policy must reach lifecycle consumers as `fail`. Required-check coverage means the named checks ran; it does not prove that every expected device was assessed.

## 4. Scope

Support one container invocation and one authoritative result. Keep execution, timeout, and reporting in NICo; keep validation logic with the provider and runtime permissions with the site. Existing native plugins retain their behavior.

Detailed schemas follow agreement on this proposal. Arbitrary scripting, streaming services, multi-container workflows, and storage protocols are outside scope.

## 5. Proposed Decision

Adopt declarative configuration as the direction for integrations that need configurable invocation and native-result interpretation. Introduce it when there is a concrete need.

For the immediate health-validation integration, use the internal DSX plugin type only if the OSS and maintenance prerequisites in Appendix B are met. Otherwise, onboard it through the proposed declarative capability. These routes share NICo's execution and reporting framework.

## 6. References

- [Pluggable Machine Validation Framework](pluggable-machine-validation-framework.md).
- [Machine Validation Reliability and Control Redesign](machine-validation-reliability-control-redesign.md).

## Appendix A Existing Native Plugin Contract

**Existing behavior:** Scout supplies `input/input.json`, and the plugin writes `output/result.json`. Both paths are under the contract directory exposed through `NICO_MV_CONTRACT_DIR`.

For example, a site chooses an eight-GPU requirement, a timeout, the Discovery context, and applicable platforms. It permits the image registry and any required privileged access; full-host access also requires approval of the exact verified revision. These controls are maintained through site configuration and the existing catalog/admin-cli workflow.

Scout generates an input document with the following structure. Identity and image values below are placeholders:

```json
{
  "contractVersion": "v1",
  "kind": "MachineValidationPluginInput",
  "runId": "<run-id>",
  "runItemId": "<run-item-id>",
  "attempt": 1,
  "machineId": "<machine-id>",
  "context": "Discovery",
  "plugin": {
    "testId": "<test-id>",
    "version": "<version>",
    "image": "registry.example.com/gpu-check@sha256:<approved-digest>"
  },
  "deadline": "2026-10-15T16:15:00Z",
  "parameters": { "expectedGpuCount": 8 }
}
```

The plugin reads the parameters, finds seven GPUs, and writes:

```json
{
  "contractVersion": "v1",
  "kind": "MachineValidationPluginResult",
  "outcome": "fail",
  "summary": "Expected 8 GPUs; found 7",
  "severity": "critical",
  "findings": [{ "name": "gpu-count", "message": "One GPU is missing" }]
}
```

The plugin exits **0** because execution and reporting completed; NICo records `fail` because a GPU is missing. A nonzero exit, timeout, or invalid/missing result is an execution error.

The plugin evaluates hardware expectations supplied through parameters. Generic required-check enforcement and warning policy are proposed additions. Under the native contract, a plugin that must block on a warning reports `fail` itself.

For health-validation under this contract, the provider would need a wrapper or modified plugin that reads NICo's input, invokes the validator, and translates its native report into `result.json`.

## Appendix B OSS Health Validator Integration

As an interim extension of Appendix A, add **`DSXMachineHealthValidation` as a plugin type**. NICo knows this type's contract and performs the translation internally, while running the upstream health-validation container unchanged. The plugin name remains a site-chosen label.

**Prerequisite:** health-validation must actually be released as OSS, with an accessible supported image and maintainable CLI/report contract, and NICo must accept compatibility ownership. This is a proposed maintenance policy; a release announcement alone is insufficient. If OSS is unavailable when onboarding is needed, use the declarative route in Appendix C.

### B.1 Existing Registration, One Additional Type

Reuse the existing registration and JSON parameters workflow:

```bash
nico-admin-cli machine-validation plugins create \
  --type DSXMachineHealthValidation \
  --name basic-machine-validation \
  --image 'nvcr.io/0837451325059433/carbide-dev/health-validation@sha256:<verified-digest>' \
  --entrypoint /health-validation \
  --entrypoint run \
  --context OnDemand \
  --platform '<platform>' \
  --parameters '{"gpu_count":8}' \
  --privileged
```

**Proposed command:** the CLI currently accepts only type `container`. Add the exact type value above to CLI/API validation and execution dispatch, and persist it with the plugin revision. Unknown types are rejected; `container` remains the default native-contract path from Appendix A.

`gpu_count` is a proposed NICo-facing parameter for this type. NICo validates it and maps it to health-validation's `checks.gpu_count.expected_gpu_count` setting. Existing image, entrypoint, timeout, context/platform, and privilege fields retain their purpose. Replace the digest and platform placeholders with verified site values.

NICo's internal implementation:

1. Reads the existing run input and parameters, generates the validator's YAML, and supplies node identity and approved NVML dependencies.
2. Runs the upstream container and captures stdout, stderr, and actual exit status.
3. Interprets its supported JSON report and returns NICo's standard outcome, summary, severity, and findings through existing reporting.

For example, a valid `overall_severity: CRITICAL` report with exit 1 becomes `fail` with critical severity. Exits 0 and 1 permit report interpretation; exit 2, an unsupported exit, an invalid report, or a timeout becomes `error`. The upstream container uses its own input/output contract; NICo supplies the translation that an ordinary native plugin would otherwise implement.

The site maintains registration values and parameters; NICo maintains the adapter and tests supported image/schema combinations. Registration, verification, access approval, and enablement follow the existing catalog workflow.

The new type does not grant extra access. NICo validates the image and entrypoint and applies site approvals. The Scout example uses privileged execution and a specific NVML mount, so full-host access is not requested.

Site configuration stays within the existing registration fields and parameters. Generic result mappings and required-check policies belong to Appendix C's declarative proposal.

### B.2 Phased Adoption of Declarative Onboarding

| Situation | Approach |
| --- | --- |
| Health-validation is OSS, NICo accepts compatibility ownership, and it is the immediate integration need | Start with type `DSXMachineHealthValidation` and the internal NICo adapter |
| Health-validation is not OSS, or its public release is still pending | Use the declarative approach for onboarding; no health-validation-specific NICo adapter |
| More plugins need configurable inputs, runtime requirements, or native-result mappings | Introduce declarative onboarding in a later phase alongside the built-in integration |

Start with the narrow OSS integration when its prerequisites are met. Add declarative onboarding alongside it when needed; it is not a prerequisite for the interim DSX type. Sites can later select a verified revision using either route while existing attempts retain their selected revision.

### B.3 Scout Runtime Experiment

The operator confirmed the following minimal command runs health-validation `0.2.0-rc1` on Scout. A supplied report for machine `7-243-98-28` uses schema `0.2` and reports CRITICAL. The container ran successfully; the machine did not pass validation.

#### Command Run on Scout

```bash
NVML_LIB=$(ldconfig -p | awk '/libnvidia-ml\.so\.1 .*=>/ {print $NF; exit}')

nerdctl --snapshotter=native -n default run \
  --rm \
  --network none \
  --privileged \
  --env LD_LIBRARY_PATH=/opt/nvml \
  -v "$NVML_LIB:/opt/nvml/libnvidia-ml.so.1:ro" \
  nvcr.io/0837451325059433/carbide-dev/health-validation:0.2.0-rc1 \
  run --node-id "$(hostname)"
```

The runtime profile in Appendix C.2 expresses the same namespace, snapshotter, permissions, environment, and NVML mount. The complete definition adds digest pinning, NICo machine identity, an optional input file, and framework-managed capture and timeout. Result rules are applied after execution; they are not nerdctl flags.

#### Sample Scout Result

This abbreviated report illustrates the assessment; NICo must validate the complete report.

```text
{
  "schema_version": "0.2",
  "catalog_version": "v0.2.0-rc1",
  "timestamp": "2026-10-13T15:55:52.277234305Z",
  "node_id": "7-243-98-28",
  "severity_by_domain": {
    "cpu": "PASS",
    "gpu": "PASS",
    "infiniband": "PASS",
    "node": "PASS",
    "nvlink": "CRITICAL",
    "xid": "PASS"
  },
  ... other fields omitted ...
  "overall_severity": "CRITICAL"
}
```

A valid full report with an accepted exit maps CRITICAL to `fail` with critical severity. The supplied full report identifies NVLink checks as affected. The actual exit status was not supplied; although the CLI contract expects 1 for CRITICAL, NICo must capture and check the real exit status.

## Appendix C Generic Declarative Onboarding

### C.1 High-Level Generic Example

**Proposed extension:** any provider can supply invocation and result rules, while the site supplies runtime approval and policy. NICo generates the tool's native input and interprets its native output; the tool does not need to implement `input.json` or `result.json`.

For a hypothetical hardware checker, a combined site definition could be:

```yaml
specVersion: v1
name: hardware-check
image: "registry.example.com/hardware-check@sha256:<approved-digest>"
runtimeProfile: site-approved-hardware
timeoutSeconds: 300
files:
  settings.yaml:
    content:
      expected_devices: 8
command:
  - /hardware-check
  - --config
  - "{inputDir}/settings.yaml"
result:
  format: json
  source: stdout
  schema: hardware-check-report-v1
  exitCodes:
    0: report
    1: report
    2: error
  field: /status
  pass: [OK]
  warn: [DEGRADED]
  fail: [FAILED]
  unknown: [UNKNOWN]
policy:
  warningBlocksAllocation: true
```

The provider owns the command, mappings, and report schema. The site owns the eight-device expectation, runtime profile, and warning policy, and imports them as one revision using Section 2.2's proposed workflow.

If the tool exits 1 and emits `{"status":"DEGRADED"}`, NICo reads the report, maps it to warning, and records `fail` because this site blocks warnings. Exit 2, an undeclared exit, or an invalid report produces `error`.

Sections C.2–C.4 show a complete health-validation example using this declarative route. Unlike the interim DSX type, it supplies invocation and interpretation rules as configuration.

Replace `<verified-digest>` with the digest of the tested `0.2.0-rc1` image. The eight-GPU expectation, required-check list, timeout, and warning policy are illustrative site choices.

### C.2 Site Runtime Configuration

The site administrator maintains this YAML in the site's deployment/configuration repository. Its containerd settings are provisioned to Scout; the approved profile is recorded with the plugin revision.

```yaml
# scout-runtime.yaml
containerRuntime:
  namespace: default
  snapshotter: native

pluginRuntimeProfiles:
  health-validation:
    privileged: true
    network: none
    hostValues:
      NVML_LIB:
        sharedLibrary: libnvidia-ml.so.1
    environment:
      LD_LIBRARY_PATH: /opt/nvml
    mounts:
      - source: "{host:NVML_LIB}"
        target: /opt/nvml/libnvidia-ml.so.1
        readOnly: true
```

`hostValues.NVML_LIB` asks Scout to locate a compatible host library; `{host:NVML_LIB}` becomes the read-only mount source. `LD_LIBRARY_PATH` is passed into the container. Host discovery and container environment variables are separate, and neither requires shell expressions in provider configuration. A missing or unusable dependency is an execution error.

This profile matches the minimal Scout invocation. A site requiring journal-based checks must verify and approve their additional dependencies.

### C.3 Complete Plugin Configuration

The provider maintains the command and result rules with its released specification. The site administrator maintains this complete site-specific definition, combining those rules with the site's values, and imports it into the NICo plugin catalog.

```yaml
# health-validation-plugin.yaml
specVersion: v1
name: health-validation
image: "nvcr.io/0837451325059433/carbide-dev/health-validation@sha256:<verified-digest>"
runtimeProfile: health-validation
timeoutSeconds: 900

environment:
  VALIDATOR_NODE_ID: "{machineId}"

files:
  operator.yaml:
    content:
      collection_timeout_seconds: 60
      checks:
        gpu_count:
          expected_gpu_count: 8

command:
  - /health-validation
  - run
  - --node-id
  - "{env:VALIDATOR_NODE_ID}"
  - --config
  - "{inputDir}/operator.yaml"

result:
  format: json
  source: stdout
  schema: health-validation-node-verdict-0.2
  exitCodes:
    0: report
    1: report
    2: error
  field: /overall_severity
  pass: [PASS, INFO]
  warn: [WARNING]
  fail: [CRITICAL]
  unknown: [UNKNOWN]
  failSeverity: critical

coverage:
  results: /check_results
  skipped: /skipped_checks
  idField: check_id
  requiredChecks:
    - gpu_count
    - nvlink_active

policy:
  warningBlocksAllocation: false
```

`VALIDATOR_NODE_ID` receives `{machineId}`; `{env:VALIDATOR_NODE_ID}` passes that resolved value as an argument. Scout serializes `files.operator.yaml.content` as YAML and mounts the input directory read-only. `{inputDir}/operator.yaml` supplies the config path.

The result rules declare the JSON source, schema, selected field, and mappings described in Section 3. `failSeverity: critical` preserves health-validation's CRITICAL classification. The input content is supplied directly; attach the full report schema to the revision.

### C.4 Onboarding and Execution

1. **Prepare.** Obtain the provider definition, full report schema, representative reports, and verified image digest.
2. **Configure.** Set the site's GPU expectation, required checks, timeout, warning policy, runtime profile, and context/platform selection.
3. **Register.** Import through the proposed admin-cli workflow in Section 2.2, then verify, approve access, and enable the revision.
4. **Execute.** Scout resolves NVML, sets the node identity, generates `operator.yaml`, and runs the container with captured stdout, stderr, and exit status.
5. **Interpret.** NICo applies the declared report mappings, required-check coverage, and site policy, then records the final outcome.

The command in Appendix B.3 was confirmed to run on Scout. The generated input file and declarative onboarding are proposed extensions, not additional functionality exercised in that manual run.
