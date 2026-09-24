#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Keep every CI job accounted for by its final required check.

GitHub gives a final job the result of each job in its `needs` list, but a
skipped job does not fail a required check. GitHub also does not tell the final
job whether that skip was expected.

This script checks both parts of that contract:

* `inventory` makes sure every top-level job feeds the final gate or has a
  reviewed exemption.
* `results` makes sure every job succeeded, unless the workflow already
  recorded a reason for that job to skip.

Missing jobs, missing decisions, failures, cancellations, and unexpected skips
all fail the final gate.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from collections import Counter
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path

# This is intentionally not a general YAML parser. We only accept the workflow
# layout needed to find root jobs, `gate_job.if`, and `gate_job.needs`. If that
# layout changes, failing here is safer than silently missing a job and letting
# the required check approve it.
JOB_KEY = re.compile(r"^  (?P<job>[A-Za-z_][A-Za-z0-9_-]*):(?:\s*#.*)?$")
NEEDS_ITEM = re.compile(r"^      - (?P<job>[A-Za-z_][A-Za-z0-9_-]*)(?:\s*#.*)?$")
GATE_IF = re.compile(r"^    if:\s*(?P<condition>.*)$")
PR_REF_PREFIX = "refs/heads/pull-request/"
UPSTREAM_REPOSITORY = "NVIDIA/infra-controller"


# Workflow inventory


class WorkflowFormatError(ValueError):
    """The workflow does not use the job layout this checker can verify."""


@dataclass(frozen=True)
class WorkflowGate:
    """Describe the final required job in one workflow."""

    display_name: str
    gate_job: str
    exemptions: Mapping[str, str]


CORE_GATE = WorkflowGate(
    display_name="Core CI",
    gate_job="core-ci-pass",
    exemptions={
        "build-summary": (
            "This reporting-only job writes the Actions summary and does not "
            "validate build output."
        ),
        "notify-build-status": (
            "This administrative job reports the completed build to Slack on "
            "protected refs."
        ),
        "core-ci-pass": "A job cannot depend on itself.",
    },
)

REST_GATE = WorkflowGate(
    display_name="REST CI",
    gate_job="rest-ci-pass",
    exemptions={"rest-ci-pass": "A job cannot depend on itself."},
)

WORKFLOW_GATES: Mapping[str, WorkflowGate] = {
    "core": CORE_GATE,
    "rest": REST_GATE,
}


@dataclass(frozen=True)
class WorkflowInventory:
    """The top-level jobs and final gate dependencies in one workflow.

    `jobs` keeps every top-level job ID in declaration order. `gate_needs`
    keeps the gate's direct dependencies in their written order, including
    duplicates, so the inventory check can report invalid entries instead of
    normalizing them.
    """

    jobs: tuple[str, ...]
    gate_needs: tuple[str, ...]


def _leading_spaces(line: str) -> int:
    """Return the number of literal spaces at the start of `line`."""

    return len(line) - len(line.lstrip(" "))


def _find_jobs(lines: list[str]) -> tuple[list[str], dict[str, int]]:
    """Return every top-level job and its first line in the workflow."""

    jobs_blocks = [index for index, line in enumerate(lines) if line == "jobs:"]
    if len(jobs_blocks) != 1:
        raise WorkflowFormatError(
            f"expected one root `jobs` block, found {len(jobs_blocks)}"
        )

    jobs: list[str] = []
    positions: dict[str, int] = {}
    for index in range(jobs_blocks[0] + 1, len(lines)):
        line = lines[index]
        if not line.strip() or line.lstrip().startswith("#"):
            continue

        indentation = _leading_spaces(line)
        if indentation == 0:
            break
        if indentation != 2:
            continue

        match = JOB_KEY.fullmatch(line)
        if match is None:
            raise WorkflowFormatError(
                f"line {index + 1} is not a supported top-level job declaration: {line!r}"
            )

        job = match.group("job")
        if job in positions:
            raise WorkflowFormatError(f"top-level job `{job}` is declared more than once")
        jobs.append(job)
        positions[job] = index

    if not jobs:
        raise WorkflowFormatError("the root `jobs` block contains no top-level jobs")

    return jobs, positions


def _parse_gate(
    lines: list[str],
    jobs: list[str],
    positions: Mapping[str, int],
    gate_job: str,
) -> list[str]:
    """Read the final gate and require the form this checker can protect.

    `if: always()` is part of the contract. Without it, an upstream failure
    skips the final check before it can report which dependency failed.
    """

    gate_start = positions.get(gate_job)
    if gate_start is None:
        raise WorkflowFormatError(f"the workflow does not define `{gate_job}`")

    gate_index = jobs.index(gate_job)
    gate_end = (
        positions[jobs[gate_index + 1]] if gate_index + 1 < len(jobs) else len(lines)
    )

    gate_conditions = [
        match.group("condition")
        for index in range(gate_start + 1, gate_end)
        if (match := GATE_IF.fullmatch(lines[index])) is not None
    ]
    if len(gate_conditions) != 1:
        raise WorkflowFormatError(
            f"expected one gate-level `if` on `{gate_job}`, "
            f"found {len(gate_conditions)}"
        )
    if gate_conditions[0] != "always()":
        raise WorkflowFormatError(
            f"`{gate_job}.if` must be `always()`, found {gate_conditions[0]!r}"
        )

    needs_lines = [
        index
        for index in range(gate_start + 1, gate_end)
        if lines[index] == "    needs:"
    ]
    if len(needs_lines) != 1:
        raise WorkflowFormatError(
            f"expected one block-style `needs` list on `{gate_job}`, "
            f"found {len(needs_lines)}"
        )

    needs: list[str] = []
    for index in range(needs_lines[0] + 1, gate_end):
        line = lines[index]
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        indentation = _leading_spaces(line)
        if indentation == 4 and line.startswith("    - "):
            raise WorkflowFormatError(
                f"line {index + 1} must indent `{gate_job}.needs` items "
                f"by six spaces: {line!r}"
            )
        if indentation <= 4:
            break

        match = NEEDS_ITEM.fullmatch(line)
        if match is None:
            raise WorkflowFormatError(
                f"line {index + 1} is not a supported `{gate_job}.needs` item: "
                f"{line!r}"
            )
        needs.append(match.group("job"))

    if not needs:
        raise WorkflowFormatError(f"`{gate_job}.needs` contains no jobs")

    return needs


def parse_workflow(workflow_text: str, gate_job: str) -> WorkflowInventory:
    """Parse the small part of a GitHub Actions workflow the gate relies on."""

    lines = workflow_text.splitlines()
    jobs, positions = _find_jobs(lines)
    gate_needs = _parse_gate(lines, jobs, positions, gate_job)
    return WorkflowInventory(jobs=tuple(jobs), gate_needs=tuple(gate_needs))


def _inventory_errors(
    inventory: WorkflowInventory, exemptions: Mapping[str, str]
) -> list[str]:
    """Explain invalid classifications in an already parsed inventory."""

    errors: list[str] = []
    jobs = set(inventory.jobs)
    gated_jobs = set(inventory.gate_needs)
    exempt_jobs = set(exemptions)

    duplicate_needs = sorted(
        job for job, count in Counter(inventory.gate_needs).items() if count > 1
    )
    if duplicate_needs:
        errors.append(
            "gate dependencies are listed more than once: " + ", ".join(duplicate_needs)
        )

    unknown_needs = sorted(gated_jobs - jobs)
    if unknown_needs:
        errors.append(
            "gate dependencies are not top-level jobs: " + ", ".join(unknown_needs)
        )

    unknown_exemptions = sorted(exempt_jobs - jobs)
    if unknown_exemptions:
        errors.append(
            "exemptions are not top-level jobs: " + ", ".join(unknown_exemptions)
        )

    empty_reasons = sorted(job for job, reason in exemptions.items() if not reason.strip())
    if empty_reasons:
        errors.append("exemptions need a reason: " + ", ".join(empty_reasons))

    duplicate_classifications = sorted(gated_jobs & exempt_jobs)
    if duplicate_classifications:
        errors.append(
            "jobs cannot be both gated and exempt: "
            + ", ".join(duplicate_classifications)
        )

    missing_jobs = sorted(jobs - gated_jobs - exempt_jobs)
    if missing_jobs:
        errors.append("top-level jobs are not gated or exempt: " + ", ".join(missing_jobs))

    return errors


def inventory_errors(workflow_text: str, gate: WorkflowGate) -> list[str]:
    """Parse the workflow and explain every invalid gate classification."""

    try:
        inventory = parse_workflow(workflow_text, gate.gate_job)
    except WorkflowFormatError as error:
        return [str(error)]

    return _inventory_errors(inventory, gate.exemptions)


# Recorded run decisions


class GateInputError(ValueError):
    """The workflow did not explain which jobs were allowed to skip."""


# Most Core jobs run whenever `run_core_ci` is true. The jobs below have an
# additional `if` condition controlled by one of `prepare`'s outputs. Keeping
# only these exceptions here means we do not need a second list of all Core
# jobs.
CORE_JOBS_BY_PREPARE_OUTPUT: Mapping[str, frozenset[str]] = {
    "build_container_x86_64_run": frozenset({"build-container-x86_64"}),
    "build_container_aarch64_run": frozenset({"build-container-aarch64"}),
    "runtime_container_x86_64_run": frozenset({"build-runtime-container-x86_64"}),
    "runtime_container_aarch64_run": frozenset({"build-runtime-container-aarch64"}),
    "build_artifacts_container_x86_64_run": frozenset(
        {"build-artifacts-container-x86_64"}
    ),
    "build_artifacts_container_aarch64_run": frozenset(
        {"build-artifacts-container-aarch64"}
    ),
    "publish_images": frozenset(
        {
            "merge-manifests-nvmetal-carbide",
            "merge-manifests-forge-cli",
            "merge-manifests-machine-validation",
            "build-push-helm-chart",
            "build-push-helm-prereqs-chart",
            "build-push-bluefield-helm-charts",
            "promote-to-be-scanned-image",
        }
    ),
    "source_files_changed": frozenset(
        {"build-machine-a-tron", "build-mat-k8s-controller", "lint-police"}
    ),
    "proto_files_changed": frozenset({"proto-police"}),
    "core_rpc_proto_files_changed": frozenset({"check-rest-core-proto-sync"}),
}

CORE_PR_ONLY_JOBS = frozenset({"lint-police", "migration-police", "proto-police"})
CORE_UPSTREAM_ONLY_JOB = "promote-to-be-scanned-image"


def _read_boolean_output(
    job: str, details: Mapping[str, object], output_name: str
) -> bool:
    """Read one `true` or `false` output from a completed job."""

    outputs = details.get("outputs")
    if not isinstance(outputs, Mapping):
        raise GateInputError(f"`{job}` did not provide a job outputs object")
    value = outputs.get(output_name)
    if not isinstance(value, str) or value not in {"true", "false"}:
        raise GateInputError(
            f"`{job}` output `{output_name}` must be 'true' or 'false', "
            f"found {value!r}"
        )
    return value == "true"


def _is_pull_request_run(environment: Mapping[str, str]) -> bool:
    """Return whether `GITHUB_REF` names a pull request mirror branch."""

    ref = environment.get("GITHUB_REF")
    if not ref:
        raise GateInputError("`GITHUB_REF` is not set")
    return ref.startswith(PR_REF_PREFIX)


def _workflow_should_run(job_results: Mapping[str, object], output_name: str) -> bool:
    """Read `run_core_ci` or `run_rest_ci` from the `changes` job."""

    changes = job_results.get("changes")
    if not isinstance(changes, Mapping):
        raise GateInputError("`changes` did not provide a job result object")
    if changes.get("result") != "success":
        raise GateInputError("`changes` was unexpectedly skipped")
    return _read_boolean_output("changes", changes, output_name)


def _core_jobs_allowed_to_skip(
    job_results: Mapping[str, object], environment: Mapping[str, str]
) -> set[str]:
    """Return the Core jobs that did not need to run."""

    should_run = _workflow_should_run(job_results, "run_core_ci")
    is_pull_request = _is_pull_request_run(environment)
    if not should_run:
        if not is_pull_request:
            raise GateInputError(
                "`changes.run_core_ci` was false outside a pull request"
            )
        # A REST-only PR skips Core, but `migration-police` still runs for the
        # pull request as a whole.
        return set(job_results) - {"changes", "migration-police"}

    prepare = job_results.get("prepare")
    if not isinstance(prepare, Mapping) or prepare.get("result") != "success":
        raise GateInputError("`prepare` was unexpectedly skipped")

    allowed: set[str] = set()
    # Use the decisions that `prepare` already made. Re-running path filters or
    # release logic here could disagree with the jobs we are checking.
    for output_name, jobs in CORE_JOBS_BY_PREPARE_OUTPUT.items():
        if not _read_boolean_output("prepare", prepare, output_name):
            allowed.update(jobs)

    if not is_pull_request:
        allowed.update(CORE_PR_ONLY_JOBS)

    repository = environment.get("GITHUB_REPOSITORY")
    if not repository:
        raise GateInputError("`GITHUB_REPOSITORY` is not set")
    if repository != UPSTREAM_REPOSITORY:
        allowed.add(CORE_UPSTREAM_ONLY_JOB)
    return allowed


def _rest_jobs_allowed_to_skip(
    job_results: Mapping[str, object], environment: Mapping[str, str]
) -> set[str]:
    """Return the REST jobs that did not need to run."""

    should_run = _workflow_should_run(job_results, "run_rest_ci")
    is_pull_request = _is_pull_request_run(environment)
    if not should_run:
        if not is_pull_request:
            raise GateInputError(
                "`changes.run_rest_ci` was false outside a pull request"
            )
        return set(job_results) - {"changes"}

    # Both reusable build callers are direct gate dependencies, but the ref
    # selects exactly one of them for a given run.
    if is_pull_request:
        return {"build-and-push"}
    return {"build-and-push-pr"}


def result_errors(
    job_results: Mapping[str, object],
    workflow_name: str,
    *,
    environment: Mapping[str, str] | None = None,
) -> list[str]:
    """Return every reason the Core or REST final gate must fail."""

    if not job_results:
        return ["the gate received no job results"]

    skipped_jobs: set[str] = set()
    errors: list[str] = []
    for job, details in sorted(job_results.items()):
        if not isinstance(details, Mapping):
            errors.append(f"`{job}` did not provide a job result object")
            continue

        result = details.get("result")
        if result == "failure":
            errors.append(f"`{job}` failed")
        elif result == "cancelled":
            errors.append(f"`{job}` was cancelled")
        elif result == "skipped":
            skipped_jobs.add(job)
        elif result == "success":
            continue
        else:
            errors.append(f"`{job}` returned unsupported result {result!r}")
    if errors:
        # Once a dependency is unhealthy, its downstream skips need no second
        # explanation: the final check is already guaranteed to fail.
        return errors

    github_environment = os.environ if environment is None else environment
    try:
        if workflow_name == "core":
            allowed_skips = _core_jobs_allowed_to_skip(
                job_results, github_environment
            )
        elif workflow_name == "rest":
            allowed_skips = _rest_jobs_allowed_to_skip(
                job_results, github_environment
            )
        else:
            return [f"unknown workflow {workflow_name!r}"]
    except GateInputError as error:
        return [str(error)]

    # An optional job that ran anyway has already succeeded, so only unexpected
    # skips can still make this run fail.
    return [
        f"`{job}` was unexpectedly skipped; if this is intentional, update the "
        "skip rules in `.github/ci/check_ci_gate.py`"
        for job in sorted(skipped_jobs - allowed_skips)
    ]


# Command-line entry points


def _print_annotations(errors: list[str]) -> None:
    """Write each error using GitHub Actions' workflow-command format."""

    for error in errors:
        print(f"::error::{error}")


def _check_inventory(workflow_path: Path, gate: WorkflowGate) -> int:
    """Check one workflow file and report inventory errors as annotations.

    Returns zero only when the file is readable, uses the supported layout,
    protects the gate condition, and classifies every top-level job.
    """

    try:
        workflow_text = workflow_path.read_text(encoding="utf-8")
    except OSError as error:
        _print_annotations([f"could not read `{workflow_path}`: {error}"])
        return 1

    try:
        inventory = parse_workflow(workflow_text, gate.gate_job)
    except WorkflowFormatError as error:
        _print_annotations([str(error)])
        return 1

    errors = _inventory_errors(inventory, gate.exemptions)
    if errors:
        _print_annotations(errors)
        return 1

    print(
        f"{gate.display_name} gate accounts for {len(inventory.jobs)} "
        f"top-level jobs ({len(inventory.gate_needs)} gated, "
        f"{len(gate.exemptions)} exempt)."
    )
    return 0


def _check_results(workflow_name: str) -> int:
    """Check `NEEDS_JSON` and report invalid job results as annotations.

    Each dependency is printed for the Actions log. Returns zero only when the
    input is valid and every dependency passes `result_errors`.
    """

    needs_json = os.environ.get("NEEDS_JSON")
    if needs_json is None:
        _print_annotations(["`NEEDS_JSON` is not set"])
        return 1

    try:
        job_results = json.loads(needs_json)
    except json.JSONDecodeError as error:
        _print_annotations([f"`NEEDS_JSON` is not valid JSON: {error}"])
        return 1
    if not isinstance(job_results, Mapping):
        _print_annotations(["`NEEDS_JSON` must contain a JSON object"])
        return 1

    for job, details in sorted(job_results.items()):
        result = details.get("result") if isinstance(details, Mapping) else None
        print(f"{job}: {result}")

    errors = result_errors(job_results, workflow_name)
    if errors:
        _print_annotations(errors)
        return 1

    print(
        f"All {WORKFLOW_GATES[workflow_name].display_name} jobs succeeded or skipped "
        "for a recorded reason."
    )
    return 0


def _parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    """Parse the selected check and its command-specific arguments."""

    parser = argparse.ArgumentParser(
        description="Check a CI final gate's job inventory and results."
    )
    commands = parser.add_subparsers(dest="command", required=True)

    inventory = commands.add_parser(
        "inventory", help="verify that every top-level job is gated or exempt"
    )
    inventory.add_argument(
        "--workflow",
        dest="workflow_name",
        choices=tuple(WORKFLOW_GATES),
        required=True,
        help="choose the Core or REST workflow",
    )
    inventory.add_argument("workflow_file", type=Path)

    results = commands.add_parser(
        "results", help="evaluate the job results in `NEEDS_JSON`"
    )
    results.add_argument(
        "--workflow",
        dest="workflow_name",
        choices=tuple(WORKFLOW_GATES),
        required=True,
        help="choose the Core or REST workflow",
    )
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    """Run the requested gate check."""

    args = _parse_args(argv)
    if args.command == "inventory":
        return _check_inventory(
            args.workflow_file, WORKFLOW_GATES[args.workflow_name]
        )
    return _check_results(args.workflow_name)


if __name__ == "__main__":
    sys.exit(main())
