# Machine Validation Container Plugin Adapters

**Status:** Agreed design and implementation reference, 2026-10-22. Extends the
[pluggable framework design](pluggable-machine-validation-framework.md).

## Purpose and selection

NICo already supports container plugins through a standard input/output contract.
Dedicated adapters let NICo integrate containers such as NVIDIA HealthValidation
without putting their invocation rules or output formats into the core framework.

Each immutable plugin revision carries an optional `container_plugin_adapter`.
`DSXHealthValidation` selects the HealthValidation adapter; omission selects the
standard NICo container contract. Unknown adapter names are rejected at
registration. Multiple plugins with different adapters can coexist at one site.
The adapter choice participates in qualification and verification alongside the
image digest, entrypoint, arguments, and parameters.

`type` remains `container`, both explicitly and by default. Adapter selection
belongs to each plugin revision; `[machine_validation_config]` retains site-wide
policy. Adapter selection does not introduce another execution type.

## Responsibilities and execution

| Component | Responsibility |
| --- | --- |
| Framework | Select the adapter; own scheduling, qualification, lifecycle, persistence, and reporting. |
| Adapter | Prepare the invocation and input files, identify dependencies, invoke through the shared runtime, and return a normalized result. |
| Shared runtime | Enforce site-approved access; own image pulls, container execution, timeouts, cleanup, and log capture. |

The framework passes a generic request containing machine identity, revision
settings, parameters, and execution limits. The adapter prepares the container
invocation and interprets its output. Results and logs return through existing
framework interfaces; adapters do not update controller state or persistence
directly. One initial interface and dispatch integration enables subsequent
adapters to be added through their implementations and registry entries.

## Admin CLI

```sh
nico-admin-cli machine-validation plugins create \
  --name dsx-health \
  --type container \
  --container-plugin-adapter DSXHealthValidation \
  --image 'ghcr.io/dsx-ai-factory/health-validation@sha256:<digest>' \
  --entrypoint /health-validation \
  --arg run \
  --parameters '{"gpu_count":4}' \
  --context OnDemand \
  --platform GB200
```

`--type` is optional and defaults to `container`. Repeated `--entrypoint` values
form the command vector; repeatable `--arg` values append ordered arguments.
The standard contract preserves the image ENTRYPOINT and supplies the vector
after the image reference. DSX explicitly overrides the executable with the
first vector element. Preserve this distinction for existing revisions and
execute without a shell. Command changes require a new revision and qualification.

`--parameters` carries structured input, separate from command arguments. The
standard adapter puts it into NICo's `input.json`; the DSX adapter maps optional
`gpu_count` to `checks.gpu_count.expected_gpu_count` in generated operator
configuration. The adapter appends managed arguments such as `--node-id` and
`--config` and rejects conflicting user arguments.

## HealthValidation result contract

Only `overall_severity` determines health: `PASS`, `INFO`, and `WARNING` pass;
`CRITICAL` and `UNKNOWN` fail. Preserve warning severity. Other report fields,
including schema version and individual checks, are ignored for interpretation;
the raw report remains available for diagnostics. Exits `0` and `1` permit
interpretation. Missing or unrecognized severity, malformed JSON, other process
exits, timeouts, and truncated output are execution errors.

## Runtime policy and acceptance

Registration and enablement require site approval for named adapters through
`allowed_container_plugin_adapters`, which defaults to an empty list.
`DSXHealthValidation` declares all-GPU access and container root with capabilities
dropped. Its read-only dependencies include host `journalctl`, required non-glibc
libraries, journal data, and machine identity. The shared runtime enforces this
profile without requiring privileged or writable host-root access.

Command arguments select image-specific CLI options. The tested `0.2.0-rc1`
container does not accept `--execution-context`; newer images can opt in with
`--arg=--execution-context --arg OPERATOR_INFRA`. The adapter appends only the
managed identity and configuration flags, without automatic version probing.

Validate on Scout using the tested GPU and journal setup. A valid `CRITICAL`
verdict must produce a completed, failed validation rather than an execution
error. Standard container plugins must retain their existing contract and
qualification, verification, access-approval, and enablement workflow.

Full Scout qualification remains pending. Upgrade API instances and Scouts
before introducing adapter revisions. Record the image digest, software and
hardware versions, actual-profile GPU/journal access, persisted verdicts, logs,
and timeout cleanup as described in the
[operator qualification guide](../../docs/provisioning/machine-validation.md#qualification-evidence).
