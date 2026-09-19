#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Tests for the shared CI final-gate inventory and result checks."""

from __future__ import annotations

import contextlib
import copy
import functools
import io
import itertools
import json
import os
import re
import textwrap
import unittest
from dataclasses import replace
from pathlib import Path
from unittest import mock

from check_ci_gate import (
    CORE_GATE,
    CORE_JOBS_BY_PREPARE_OUTPUT,
    CORE_PR_ONLY_JOBS,
    CORE_UPSTREAM_ONLY_JOB,
    GateInputError,
    JOB_KEY,
    WORKFLOW_GATES,
    _core_jobs_allowed_to_skip,
    inventory_errors,
    main,
    parse_workflow,
    result_errors,
)


CI_DIR = Path(__file__).resolve().parent
ROOT = CI_DIR.parents[1]
WORKFLOWS = {
    "core": ROOT / ".github/workflows/ci.yaml",
    "rest": ROOT / ".github/workflows/rest-ci.yml",
}
PREPARE_OUTPUT_REFERENCE = re.compile(
    r"needs\.prepare\.outputs\.(?P<output>[A-Za-z_][A-Za-z0-9_]*)"
)

COMPLETE_WORKFLOW = textwrap.dedent(
    """\
    name: Core fixture

    jobs:
      changes:
        runs-on: ubuntu-latest
      build:
        runs-on: ubuntu-latest
      build-summary:
        runs-on: ubuntu-latest
      notify-build-status:
        runs-on: ubuntu-latest
      core-ci-pass:
        runs-on: ubuntu-latest
        if: always()
        needs:
          - changes
          - build
        steps:
          - run: true
    """
)


@functools.cache
def _read_workflow(workflow_name: str) -> str:
    """Read the checked-in Core or REST workflow."""

    return WORKFLOWS[workflow_name].read_text(encoding="utf-8")


@functools.cache
def _read_gate_dependencies(workflow_name: str) -> tuple[str, ...]:
    """Return the jobs listed in the final gate's `needs` block."""

    gate = WORKFLOW_GATES[workflow_name]
    inventory = parse_workflow(_read_workflow(workflow_name), gate.gate_job)
    return inventory.gate_needs


def _read_job_yaml(workflow_name: str, job: str) -> str:
    """Return the YAML block for one job in the checked-in workflow."""

    lines = _read_workflow(workflow_name).splitlines()
    header = f"  {job}:"
    start = next(
        (index for index, line in enumerate(lines) if line == header), None
    )
    if start is None:
        raise AssertionError(
            f"{workflow_name} workflow does not declare job {job!r}"
        )

    body: list[str] = []
    for line in lines[start + 1 :]:
        if JOB_KEY.fullmatch(line):
            break
        body.append(line)
    return "\n".join(body)


def _read_job_condition(workflow_name: str, job: str) -> str:
    """Return one job's top-level `if` condition, including folded lines."""

    lines = _read_job_yaml(workflow_name, job).splitlines()
    for index, line in enumerate(lines):
        if not line.startswith("    if:"):
            continue
        condition = [line.partition("if:")[2].strip()]
        for continuation in lines[index + 1 :]:
            if continuation.strip() and not continuation.startswith("      "):
                break
            condition.append(continuation.strip())
        return " ".join(part for part in condition if part)
    return ""


def _build_result_context(
    workflow_name: str,
    *,
    run_ci: bool = True,
    pull_request: bool = True,
    skipped_jobs: set[str] | frozenset[str] = frozenset(),
    upstream: bool = True,
) -> tuple[dict[str, object], dict[str, str]]:
    """Build the job results and GitHub variables for one test run."""

    job_results: dict[str, object] = {
        job: {
            "result": "skipped" if job in skipped_jobs else "success",
            "outputs": {},
        }
        for job in _read_gate_dependencies(workflow_name)
    }
    run_output = "run_core_ci" if workflow_name == "core" else "run_rest_ci"
    job_results["changes"]["outputs"] = {run_output: str(run_ci).lower()}
    if workflow_name == "core":
        job_results["prepare"]["outputs"] = dict.fromkeys(
            CORE_JOBS_BY_PREPARE_OUTPUT, "true"
        )
    environment = {
        "GITHUB_REF": (
            "refs/heads/pull-request/123" if pull_request else "refs/heads/main"
        ),
        "GITHUB_REPOSITORY": (
            "NVIDIA/infra-controller" if upstream else "example/fork"
        ),
    }
    return job_results, environment


class InventoryTests(unittest.TestCase):
    """Verify that every workflow job has exactly one gate classification."""

    def test_checked_in_workflows_match_gate_configuration(self) -> None:
        for workflow_name, gate in WORKFLOW_GATES.items():
            with self.subTest(workflow_name):
                workflow = _read_workflow(workflow_name)
                self.assertEqual(inventory_errors(workflow, gate), [])

        # The Core table and the checked-in job conditions must reference each
        # other in both directions. This makes a newly conditional job fail in
        # `changes`, instead of surprising the final required check later.
        expected_output_by_job: dict[str, str] = {}
        for output_name, jobs in CORE_JOBS_BY_PREPARE_OUTPUT.items():
            for job in jobs:
                self.assertNotIn(job, expected_output_by_job)
                expected_output_by_job[job] = output_name

        actual_output_by_job: dict[str, str] = {}
        pull_request_jobs: set[str] = set()
        upstream_only_jobs: set[str] = set()
        for job in _read_gate_dependencies("core"):
            condition = _read_job_condition("core", job)
            outputs = {
                match.group("output")
                for match in PREPARE_OUTPUT_REFERENCE.finditer(condition)
            }
            self.assertLessEqual(len(outputs), 1, job)
            if outputs:
                actual_output_by_job[job] = outputs.pop()
            if "pull-request/" in condition:
                pull_request_jobs.add(job)
            if "github.repository ==" in condition:
                upstream_only_jobs.add(job)

        self.assertEqual(actual_output_by_job, expected_output_by_job)
        self.assertEqual(pull_request_jobs, set(CORE_PR_ONLY_JOBS))
        self.assertEqual(
            upstream_only_jobs,
            {CORE_UPSTREAM_ONLY_JOB},
        )
        self.assertIn(
            "!startsWith(github.ref, 'refs/heads/pull-request/')",
            _read_job_yaml("rest", "build-and-push"),
        )
        self.assertIn(
            "startsWith(github.ref, 'refs/heads/pull-request/')",
            _read_job_yaml("rest", "build-and-push-pr"),
        )

    def test_inventory_classification(self) -> None:
        cases = (
            {
                "name": "complete inventory",
                "workflow": COMPLETE_WORKFLOW,
                "gate": CORE_GATE,
                "expected": [],
            },
            {
                "name": "missing gate dependency",
                "workflow": COMPLETE_WORKFLOW.replace("      - build\n", ""),
                "gate": CORE_GATE,
                "expected": ["top-level jobs are not gated or exempt: build"],
            },
            {
                "name": "unknown exemption",
                "workflow": COMPLETE_WORKFLOW,
                "gate": replace(
                    CORE_GATE,
                    exemptions={
                        **CORE_GATE.exemptions,
                        "retired-job": "No longer used.",
                    },
                ),
                "expected": ["exemptions are not top-level jobs: retired-job"],
            },
            {
                "name": "unknown gate dependency",
                "workflow": COMPLETE_WORKFLOW.replace(
                    "      - build\n", "      - build\n      - retired-job\n"
                ),
                "gate": CORE_GATE,
                "expected": [
                    "gate dependencies are not top-level jobs: retired-job"
                ],
            },
            {
                "name": "duplicate classification",
                "workflow": COMPLETE_WORKFLOW,
                "gate": replace(
                    CORE_GATE,
                    exemptions={
                        **CORE_GATE.exemptions,
                        "build": "Fixture duplicate.",
                    },
                ),
                "expected": ["jobs cannot be both gated and exempt: build"],
            },
            {
                "name": "duplicate gate dependency",
                "workflow": COMPLETE_WORKFLOW.replace(
                    "      - build\n", "      - build\n      - build\n"
                ),
                "gate": CORE_GATE,
                "expected": ["gate dependencies are listed more than once: build"],
            },
            {
                "name": "empty exemption reason",
                "workflow": COMPLETE_WORKFLOW,
                "gate": replace(
                    CORE_GATE,
                    exemptions={**CORE_GATE.exemptions, "build-summary": ""},
                ),
                "expected": ["exemptions need a reason: build-summary"],
            },
        )

        for case in cases:
            with self.subTest(case["name"]):
                self.assertEqual(
                    inventory_errors(case["workflow"], case["gate"]),
                    case["expected"],
                )

    def test_required_workflow_sections(self) -> None:
        cases = (
            {
                "name": "missing jobs block",
                "workflow": COMPLETE_WORKFLOW.replace("jobs:\n", "pipelines:\n"),
                "expected": "expected one root `jobs` block, found 0",
            },
            {
                "name": "missing gate job",
                "workflow": COMPLETE_WORKFLOW.replace("core-ci-pass:", "retired-gate:"),
                "expected": "the workflow does not define `core-ci-pass`",
            },
            {
                "name": "missing gate needs",
                "workflow": COMPLETE_WORKFLOW.replace("    needs:\n", "    dependencies:\n"),
                "expected": (
                    "expected one block-style `needs` list on `core-ci-pass`, found 0"
                ),
            },
            {
                "name": "missing gate condition",
                "workflow": COMPLETE_WORKFLOW.replace("    if: always()\n", ""),
                "expected": (
                    "expected one gate-level `if` on `core-ci-pass`, found 0"
                ),
            },
            {
                "name": "wrong gate condition",
                "workflow": COMPLETE_WORKFLOW.replace("if: always()", "if: success()"),
                "expected": "`core-ci-pass.if` must be `always()`, found 'success()'",
            },
            {
                "name": "duplicate gate condition",
                "workflow": COMPLETE_WORKFLOW.replace(
                    "    if: always()\n",
                    "    if: always()\n    if: always()\n",
                ),
                "expected": (
                    "expected one gate-level `if` on `core-ci-pass`, found 2"
                ),
            },
            {
                "name": "duplicate top-level job",
                "workflow": COMPLETE_WORKFLOW.replace(
                    "  build:\n    runs-on: ubuntu-latest\n",
                    "  build:\n    runs-on: ubuntu-latest\n"
                    "  build:\n    runs-on: ubuntu-latest\n",
                ),
                "expected": "top-level job `build` is declared more than once",
            },
            {
                "name": "malformed top-level job",
                "workflow": COMPLETE_WORKFLOW.replace(
                    "  build:\n    runs-on: ubuntu-latest\n",
                    "  build: {runs-on: ubuntu-latest}\n",
                ),
                "expected": (
                    "line 6 is not a supported top-level job declaration: "
                    "'  build: {runs-on: ubuntu-latest}'"
                ),
            },
            {
                "name": "malformed gate dependency",
                "workflow": COMPLETE_WORKFLOW.replace("      - build\n", "      - [build]\n"),
                "expected": (
                    "line 17 is not a supported `core-ci-pass.needs` item: "
                    "'      - [build]'"
                ),
            },
            {
                "name": "empty gate dependencies",
                "workflow": COMPLETE_WORKFLOW.replace(
                    "      - changes\n      - build\n", ""
                ),
                "expected": "`core-ci-pass.needs` contains no jobs",
            },
            {
                "name": "under-indented gate dependency",
                "workflow": COMPLETE_WORKFLOW.replace(
                    "      - changes\n", "    - changes\n"
                ),
                "expected": (
                    "line 16 must indent `core-ci-pass.needs` items by six spaces: "
                    "'    - changes'"
                ),
            },
        )

        for case in cases:
            with self.subTest(case["name"]):
                self.assertEqual(
                    inventory_errors(case["workflow"], CORE_GATE),
                    [case["expected"]],
                )


class CommandTests(unittest.TestCase):
    """Verify workflow selection and file handling at the CLI boundary."""

    def test_inventory_command_accepts_each_workflow(self) -> None:
        cases = (
            ("core", "Core CI gate accounts for"),
            ("rest", "REST CI gate accounts for"),
        )

        for workflow_name, expected in cases:
            with self.subTest(workflow_name):
                output = io.StringIO()
                with contextlib.redirect_stdout(output):
                    return_code = main(
                        [
                            "inventory",
                            "--workflow",
                            workflow_name,
                            str(WORKFLOWS[workflow_name]),
                        ]
                    )

                self.assertEqual(return_code, 0)
                self.assertIn(expected, output.getvalue())

    def test_inventory_command_rejects_unknown_workflow(self) -> None:
        with contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaisesRegex(SystemExit, "2"):
                main(["inventory", "--workflow", "unknown", "workflow.yml"])

    def test_inventory_command_rejects_unreadable_workflow(self) -> None:
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            return_code = main(
                ["inventory", "--workflow", "core", "/missing/workflow.yml"]
            )

        self.assertEqual(return_code, 1)
        self.assertIn("::error::could not read", output.getvalue())


class ResultTests(unittest.TestCase):
    """Verify expected skips and every fail-closed result boundary."""

    def _assert_results_pass(
        self,
        workflow_name: str,
        *,
        run_ci: bool = True,
        pull_request: bool = True,
        skipped_jobs: set[str] | frozenset[str] = frozenset(),
        upstream: bool = True,
    ) -> None:
        job_results, environment = _build_result_context(
            workflow_name,
            run_ci=run_ci,
            pull_request=pull_request,
            skipped_jobs=skipped_jobs,
            upstream=upstream,
        )
        self.assertEqual(
            result_errors(job_results, workflow_name, environment=environment), []
        )

    def test_healthy_core_results(self) -> None:
        core_jobs = set(_read_gate_dependencies("core"))
        self._assert_results_pass("core")
        self._assert_results_pass(
            "core",
            run_ci=False,
            skipped_jobs=core_jobs - {"changes", "migration-police"},
        )
        self._assert_results_pass(
            "core", pull_request=False, skipped_jobs=set(CORE_PR_ONLY_JOBS)
        )
        self._assert_results_pass(
            "core", upstream=False, skipped_jobs={CORE_UPSTREAM_ONLY_JOB}
        )
        # A job that was allowed to skip may still run successfully.
        self._assert_results_pass("core", upstream=False)

    def test_every_rest_decision_combination(self) -> None:
        rest_jobs = set(_read_gate_dependencies("rest"))
        for run_ci, pull_request in itertools.product((False, True), repeat=2):
            if not run_ci and not pull_request:
                job_results, environment = _build_result_context(
                    "rest", run_ci=False, pull_request=False
                )
                self.assertIn(
                    "`changes.run_rest_ci` was false outside a pull request",
                    result_errors(job_results, "rest", environment=environment),
                )
            elif not run_ci:
                self._assert_results_pass(
                    "rest", run_ci=False, skipped_jobs=rest_jobs - {"changes"}
                )
            else:
                self._assert_results_pass(
                    "rest",
                    pull_request=pull_request,
                    skipped_jobs={
                        "build-and-push" if pull_request else "build-and-push-pr"
                    },
                )

    def test_migration_police_still_runs_when_core_does_not(self) -> None:
        job_results, environment = _build_result_context(
            "core",
            run_ci=False,
            skipped_jobs=set(_read_gate_dependencies("core")) - {"changes"},
        )
        self.assertIn(
            "`migration-police` was unexpectedly skipped",
            result_errors(job_results, "core", environment=environment)[0],
        )

    def test_every_core_decision_combination(self) -> None:
        """Cover every boolean input to the Core skip decision in one table."""

        output_names = tuple(CORE_JOBS_BY_PREPARE_OUTPUT)
        core_jobs = set(_read_gate_dependencies("core"))
        prepare_combinations = tuple(
            itertools.product((False, True), repeat=len(output_names))
        )

        for run_ci, pull_request, upstream in itertools.product(
            (False, True), repeat=3
        ):
            for prepare_values in prepare_combinations:
                prepare_decisions = dict(
                    zip(output_names, prepare_values, strict=True)
                )
                job_results, environment = _build_result_context(
                    "core",
                    run_ci=run_ci,
                    pull_request=pull_request,
                    upstream=upstream,
                )
                job_results["prepare"]["outputs"].update(
                    {
                        name: str(value).lower()
                        for name, value in prepare_decisions.items()
                    }
                )
                if not run_ci and not pull_request:
                    with self.assertRaisesRegex(
                        GateInputError,
                        "`changes.run_core_ci` was false outside a pull request",
                    ):
                        _core_jobs_allowed_to_skip(job_results, environment)
                    continue

                if not run_ci:
                    expected = core_jobs - {"changes", "migration-police"}
                else:
                    expected = set()
                    for output_name, value in prepare_decisions.items():
                        if not value:
                            expected.update(
                                CORE_JOBS_BY_PREPARE_OUTPUT[output_name]
                            )
                    if not pull_request:
                        expected.update(CORE_PR_ONLY_JOBS)
                    if not upstream:
                        expected.add(CORE_UPSTREAM_ONLY_JOB)

                self.assertEqual(
                    _core_jobs_allowed_to_skip(job_results, environment),
                    expected,
                    (run_ci, pull_request, upstream, prepare_decisions),
                )

    def test_every_gated_job_rejects_an_unexpected_skip(self) -> None:
        core_results, core_environment = _build_result_context("core")
        for job in _read_gate_dependencies("core"):
            with self.subTest(workflow="core", job=job):
                mutated = copy.deepcopy(core_results)
                mutated[job]["result"] = "skipped"
                self.assertIn(
                    f"`{job}` was unexpectedly skipped",
                    result_errors(
                        mutated, "core", environment=core_environment
                    )[0],
                )

        for job in _read_gate_dependencies("rest"):
            # Use the ref that requires this job. The other Docker job is the
            # one expected skip in the starting result set.
            pull_request = job != "build-and-push"
            rest_results, rest_environment = _build_result_context(
                "rest",
                pull_request=pull_request,
                skipped_jobs={
                    "build-and-push" if pull_request else "build-and-push-pr"
                },
            )
            with self.subTest(workflow="rest", job=job):
                rest_results[job]["result"] = "skipped"
                self.assertIn(
                    f"`{job}` was unexpectedly skipped",
                    result_errors(
                        rest_results, "rest", environment=rest_environment
                    )[0],
                )

    def test_bad_terminal_and_malformed_results(self) -> None:
        cases = (
            ("failure", "failure", "failed"),
            ("cancellation", "cancelled", "was cancelled"),
            ("unknown", "waiting", "unsupported result 'waiting'"),
        )
        for name, result, expected in cases:
            with self.subTest(name):
                job_results = {"fixture-job": {"result": result}}
                errors = result_errors(job_results, "rest", environment={})
                self.assertIn(expected, errors[0])

        self.assertIn(
            "did not provide a job result object",
            result_errors({"fixture-job": "success"}, "rest", environment={})[0],
        )

    def test_malformed_and_missing_boolean_outputs_fail_closed(self) -> None:
        for job, output_name in (
            ("changes", "run_core_ci"),
            ("prepare", "publish_images"),
        ):
            for value in ("yes", None):
                with self.subTest(output=output_name, value=value):
                    job_results, environment = _build_result_context("core")
                    outputs = job_results[job]["outputs"]
                    if value is None:
                        outputs.pop(output_name)
                    else:
                        outputs[output_name] = value
                    errors = result_errors(
                        job_results, "core", environment=environment
                    )
                    self.assertIn(output_name, errors[0])

    def test_missing_github_variables_fail_closed(self) -> None:
        core_results, environment = _build_result_context("core")
        for variable, expected in (
            ("GITHUB_REF", "`GITHUB_REF` is not set"),
            ("GITHUB_REPOSITORY", "`GITHUB_REPOSITORY` is not set"),
        ):
            with self.subTest(variable=variable):
                incomplete_environment = environment | {variable: ""}
                self.assertEqual(
                    result_errors(
                        core_results,
                        "core",
                        environment=incomplete_environment,
                    ),
                    [expected],
                )

    def test_workflow_cannot_be_disabled_outside_a_pull_request(self) -> None:
        job_results, environment = _build_result_context(
            "core", run_ci=False, pull_request=False
        )
        errors = result_errors(job_results, "core", environment=environment)
        self.assertIn(
            "`changes.run_core_ci` was false outside a pull request", errors
        )

    def test_missing_workflow_decisions_fail_closed(self) -> None:
        job_results, environment = _build_result_context("core")
        job_results["prepare"]["result"] = "skipped"
        self.assertIn(
            "`prepare` was unexpectedly skipped",
            result_errors(job_results, "core", environment=environment)[0],
        )

        job_results, environment = _build_result_context("core")
        job_results["changes"].pop("outputs")
        self.assertIn(
            "did not provide a job outputs object",
            result_errors(job_results, "core", environment=environment)[0],
        )

    def test_unknown_workflow_fails_closed(self) -> None:
        core_results, environment = _build_result_context("core")
        self.assertEqual(
            result_errors(core_results, "retired", environment=environment),
            ["unknown workflow 'retired'"],
        )

    def test_workflow_only_regression(self) -> None:
        # #4324 was possible because a workflow-only change skipped
        # `lint-police` and `core-ci-pass` accepted that skip. The source
        # filter selects lint; this assertion makes sure the final gate rejects
        # the old result.
        job_results, environment = _build_result_context("core")
        job_results["lint-police"]["result"] = "skipped"
        self.assertIn(
            "`lint-police` was unexpectedly skipped",
            result_errors(job_results, "core", environment=environment)[0],
        )

    def test_result_command_accepts_healthy_context(self) -> None:
        job_results, environment = _build_result_context(
            "rest", pull_request=False, skipped_jobs={"build-and-push-pr"}
        )
        process_environment = {
            **environment,
            "NEEDS_JSON": json.dumps(job_results),
        }
        output = io.StringIO()
        with mock.patch.dict(
            os.environ, process_environment, clear=True
        ), contextlib.redirect_stdout(output):
            return_code = main(["results", "--workflow", "rest"])

        self.assertEqual(return_code, 0)
        self.assertIn("All REST CI jobs succeeded or skipped", output.getvalue())

    def test_result_command_rejects_invalid_context(self) -> None:
        cases = (
            ("malformed", "{", "is not valid JSON"),
            ("empty", "{}", "received no job results"),
            ("non-mapping", "[]", "must contain a JSON object"),
        )
        for name, needs_json, expected in cases:
            with self.subTest(name):
                output = io.StringIO()
                with mock.patch.dict(
                    os.environ, {"NEEDS_JSON": needs_json}, clear=True
                ), contextlib.redirect_stdout(output):
                    return_code = main(["results", "--workflow", "rest"])

                self.assertEqual(return_code, 1)
                self.assertIn(expected, output.getvalue())

    def test_result_command_rejects_missing_context(self) -> None:
        output = io.StringIO()
        with mock.patch.dict(
            os.environ, {}, clear=True
        ), contextlib.redirect_stdout(output):
            return_code = main(["results", "--workflow", "core"])

        self.assertEqual(return_code, 1)
        self.assertIn("::error::`NEEDS_JSON` is not set", output.getvalue())


if __name__ == "__main__":
    unittest.main()
