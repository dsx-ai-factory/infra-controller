// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

const assert = require("node:assert/strict");
const { test } = require("node:test");
const { mkdtempSync, readFileSync, rmSync, writeFileSync } = require("node:fs");
const { tmpdir } = require("node:os");
const { join } = require("node:path");
const { spawnSync } = require("node:child_process");
const { authorizePush } = require("./pr_push.cjs");

const currentSha = "a".repeat(40);
const previousSha = "b".repeat(40);
const request = { owner: "example", repo: "controller", prNumber: 123, commentId: 456 };

/**
 * Provide GitHub responses for one admitted PR so each case can change the
 * specific authorization or commit condition that must allow or reject publishing.
 */
function githubFor(overrides = {}) {
  const values = {
    body: "/ok to push " + currentSha,
    issue: "/repos/example/controller/issues/123",
    permission: "write",
    state: "open",
    head: currentSha,
    mirror: currentSha,
    missingMirror: false,
    ...overrides,
  };
  return {
    rest: {
      issues: {
        getComment: async () => ({ data: {
          body: values.body,
          issue_url: "https://api.github.com" + values.issue,
          user: { login: "maintainer" },
        } }),
      },
      repos: {
        getCollaboratorPermissionLevel: async () => ({ data: {
          permission: values.permission,
        } }),
      },
      pulls: {
        get: async () => ({ data: {
          state: values.state,
          head: { sha: values.head },
          base: { repo: { full_name: "example/controller" } },
        } }),
      },
      git: {
        getRef: async () => {
          if (values.missingMirror) throw Object.assign(new Error("Not Found"), { status: 404 });
          return { data: { object: { sha: values.mirror } } };
        },
      },
    },
  };
}

/**
 * Accept explicit and current-head requests from privileged users so maintainers
 * can publish a reviewed revision without approving or merging the PR.
 */
test("privileged requests authorize the exact admitted commit", async (t) => {
  const cases = [
    // Write access admits a SHA-specific request, including dispatch revalidation.
    { name: "explicit SHA", overrides: {}, expectedSha: currentSha },
    // A bare command resolves the current head rather than requiring a commit argument.
    { name: "current head", overrides: { body: "/ok to push" }, expectedSha: currentSha },
    // Admin permission remains eligible alongside GitHub's Write/Maintain base permission.
    { name: "administrator", overrides: { permission: "admin" }, expectedSha: currentSha },
  ];
  for (const scenario of cases) {
    await t.test(scenario.name, async () => {
      // Change only the condition named by the case; the remaining responses describe an admitted PR.
      const github = githubFor(scenario.overrides);

      // Authorize the request using the SHA captured before dispatch.
      const approved = await authorizePush({ github, ...request, expectedSha: scenario.expectedSha });

      // Return one immutable commit and its bot-managed branch for the build.
      assert.deepEqual(approved, { ref: "pull-request/123", sha: currentSha });
    });
  }
});

/**
 * Reject invalid authority and changed PR state so a dispatch cannot publish
 * another revision, replay another PR's approval, or bypass bot admission.
 */
test("publishing rejects invalid or stale requests", async (t) => {
  const cases = [
    // A command must occupy the comment, preventing quoted discussion from authorizing a build.
    { name: "embedded command", overrides: { body: "Please run /ok to push" }, error: /Use \/ok to push/ },
    // An abbreviated revision cannot ambiguously authorize a different commit.
    { name: "short SHA", overrides: { body: "/ok to push abc1234" }, error: /full lowercase/ },
    // Organization membership or Read access alone does not grant publishing permission.
    { name: "read permission", overrides: { permission: "read" }, error: /requires Write/ },
    // A comment ID from another PR must not authorize this PR's code.
    { name: "different PR", overrides: { issue: "/repos/example/controller/issues/124" }, error: /another PR/ },
    // Closing a PR invalidates a queued request before the build starts.
    { name: "closed PR", overrides: { state: "closed" }, error: /open PR/ },
    // An explicit SHA must still name the PR head when authorization is checked.
    { name: "old explicit SHA", overrides: { body: "/ok to push " + previousSha }, error: /PR head changed/ },
    // A bare command cannot follow a new head between request handling and dispatch execution.
    { name: "head changed after dispatch", overrides: { body: "/ok to push" }, expectedSha: previousSha, error: /PR head changed/ },
    // A stale bot branch must not substitute an earlier revision for the requested head.
    { name: "stale mirror", overrides: { mirror: previousSha }, error: /mirror is stale/ },
    // A missing bot branch must give the maintainer the prerequisite admission command.
    { name: "missing mirror", overrides: { missingMirror: true }, error: /\/ok to test/ },
  ];
  for (const scenario of cases) {
    await t.test(scenario.name, async () => {
      // Change one trust or commit condition while keeping the other GitHub responses valid.
      const github = githubFor(scenario.overrides);

      // Reject the request before a caller can use the returned commit to dispatch a build.
      await assert.rejects(
        authorizePush({ github, ...request, expectedSha: scenario.expectedSha }),
        scenario.error,
      );
    });
  }
});

/**
 * Propagate GitHub authorization failures so an unavailable permission check
 * cannot be mistaken for permission to publish a PR.
 */
test("GitHub permission failures reject publishing", async () => {
  // Inject a permission-service failure while the PR and mirror remain admitted.
  const github = githubFor();
  github.rest.repos.getCollaboratorPermissionLevel = async () => {
    throw new Error("GitHub unavailable");
  };

  // Reject the operation instead of continuing with an unverified commenter.
  await assert.rejects(authorizePush({ github, ...request }), /GitHub unavailable/);
});

/**
 * Execute a named workflow script with controlled external commands so tests
 * verify the shipped tag and manifest behavior rather than a copy of its logic.
 */
function runWorkflowScript(t, { workflow, section, field, indent, environment, prelude = "" }) {
  // Stop at the next section to avoid accidentally testing another job's script.
  const lines = readFileSync(join(__dirname, "../workflows", workflow), "utf8").split("\n");
  const start = lines.indexOf(section);
  assert.ok(start >= 0, section);
  const sectionIndent = section.search(/\S/);
  let end = start + 1;
  while (end < lines.length && (!lines[end].trim() || lines[end].search(/\S/) > sectionIndent)) end++;
  const block = lines.slice(start + 1, end);
  const scriptStart = block.findIndex((line) => line.trim() === field + ": |");
  assert.ok(scriptStart >= 0, field);
  const script = [];
  for (const line of block.slice(scriptStart + 1)) {
    if (line.trim() && !line.startsWith(" ".repeat(indent))) break;
    script.push(line.slice(indent));
  }

  // Give each execution its own output file and remove it after the assertions.
  const directory = mkdtempSync(join(tmpdir(), "pr-push-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const output = join(directory, "output");
  writeFileSync(output, "");
  const result = spawnSync("bash", ["-c", prelude + "\n" + script.join("\n")], {
    encoding: "utf8",
    env: { ...process.env, GITHUB_OUTPUT: output, OUTPUT: output, ...environment },
  });
  return { ...result, output: readFileSync(output, "utf8") };
}

/**
 * Accept a numeric short SHA beginning with zero during PR version calculation
 * so an admitted revision can reach the image builds.
 */
test("PR version calculation accepts a numeric short SHA beginning with zero", (t) => {
  // Supply 0958893 as the short SHA. The other Git outputs provide valid tagged history.
  const result = runWorkflowScript(t, {
    workflow: "ci.yaml", section: "      - name: Calculate version",
    field: "run", indent: 10,
    environment: {
      PR_PUBLISH: "true", // Select the PR suffix appended after Git version normalization.
      GITHUB_REF_NAME: "pull-request/123",
      GITHUB_STEP_SUMMARY: "/dev/null",
    },
    prelude: `
      git() {
        case "$*" in
          "rev-parse --short=7 HEAD") printf '%s\\n' '0958893' ;;
          "log -1 --format=%cI") printf '%s\\n' '2026-10-29T12:00:00+00:00' ;;
          "describe --tags --first-parent --always")
            printf '%s\\n' 'v2.5.0-pr-194-g09588930e' ;;
          *) return 1 ;;
        esac
      }
    `,
  });

  // Preserve the raw SHA output while emitting PR versions accepted by strict validation.
  assert.equal(result.status, 0, result.stdout + result.stderr);
  const versionOutputs = result.output.trim().split("\n");
  assert.ok(versionOutputs.includes("short_sha=0958893"));
  assert.ok(versionOutputs.includes(
    "version=v2.5.0-pr-194-g09588930e-pr.123.g0958893",
  ));
  assert.ok(versionOutputs.includes(
    "helm_version=2.5.0-pr-194.g09588930e-pr.123.g0958893",
  ));
});

/**
 * Select base images from the authorized dispatch commit or push event message
 * so rebuild requests survive PR publishing without changing ordinary pushes.
 */
test("base-container decisions preserve event-specific rebuild requests", async (t) => {
  const imageVersion = "v2.5.0-pr.123.gaaaaaaa";
  const targetRegistry = "registry.example/target";
  const sourceRegistry = "registry.example/source";
  const rebuildMessage = "Refresh base dependencies\n\nci-rebuild-base-containers";
  const baseImages = [
    "build-container-x86_64",
    "runtime-container-x86_64",
    "build-container-aarch64",
    "runtime-container-aarch64",
    "build-artifacts-container-x86_64",
    "build-artifacts-container-aarch64",
  ];
  const cases = [
    // Dispatches must recover markers in the captured commit body without a push payload.
    {
      name: "dispatch rebuild override", eventName: "workflow_dispatch",
      eventMessage: "", capturedMessage: rebuildMessage, rebuild: true,
    },
    // Ordinary captured messages must retain base reuse rather than rebuilding every dispatch.
    {
      name: "dispatch without override", eventName: "workflow_dispatch",
      eventMessage: "", capturedMessage: "Update source code", rebuild: false,
    },
    // Push overrides must keep working without consulting Git for another message.
    {
      name: "push rebuild override", eventName: "push",
      eventMessage: rebuildMessage, rebuild: true,
    },
    // Empty push messages must remain empty instead of acquiring the dispatch fallback.
    {
      name: "push without event message", eventName: "push",
      eventMessage: "", rebuild: false,
    },
    // Failed required reads must stop before publication can consume fallback base decisions.
    {
      name: "dispatch commit lookup fails", eventName: "workflow_dispatch",
      eventMessage: "", lookupFails: true,
      /** Verify failed reads stop the gate before fallback decisions can reach publication. */
      check(result) {
        assert.equal(result.status, 1, result.stdout + result.stderr);
        assert.equal(result.output, "");
      },
    },
  ];
  for (const scenario of cases) {
    await t.test(scenario.name, (t) => {
      // Leave watched paths unchanged so the message source alone controls rebuilding.
      const result = runWorkflowScript(t, {
        workflow: "ci.yaml", section: "      - name: Decide if build-container-x86_64 must run",
        field: "run", indent: 10,
        environment: {
          TARGET_REGISTRY: targetRegistry, SOURCE_REGISTRY: sourceRegistry,
          VERSION: imageVersion, GITHUB_SHA: currentSha,
          GITHUB_REF: "refs/heads/pull-request/123",
          EVENT_NAME: scenario.eventName, // Select dispatch recovery or push payload semantics.
          COMMIT_MESSAGE: scenario.eventMessage, // Supply the event message independently of Git.
          CAPTURED_COMMIT_MESSAGE: scenario.capturedMessage || "",
          FAIL_COMMIT_LOOKUP: String(scenario.lookupFails || false),
          BUILD_CONTAINER_X86_64_DOCKERFILE_CHANGED: "false",
          RUNTIME_CONTAINER_X86_64_DOCKERFILE_CHANGED: "false",
          RUNTIME_CONTAINER_AARCH64_DOCKERFILE_CHANGED: "false",
          BUILD_CONTAINER_AARCH64_DOCKERFILE_CHANGED: "false",
          BUILD_ARTIFACTS_X86_64_DOCKERFILE_CHANGED: "false",
          BUILD_ARTIFACTS_AARCH64_DOCKERFILE_CHANGED: "false",
          PROTO_FILES_CHANGED: "false", CORE_RPC_PROTO_FILES_CHANGED: "false",
          SOURCE_FILES_CHANGED: "false", MACHINE_LIFECYCLE_CHANGED: "false",
          CONTROL_PLANE_DPU_CHANGED: "false",
        },
        prelude: `
          # Reject push lookups and require the captured SHA for dispatch metadata.
          git() {
            [[ "$EVENT_NAME" == "workflow_dispatch" ]] || return 1
            [[ "$*" == "log -1 --format=%B $GITHUB_SHA" ]] || return 1
            [[ "$FAIL_COMMIT_LOOKUP" != "true" ]] || return 1
            printf '%s\\n' "$CAPTURED_COMMIT_MESSAGE"
          }
        `,
      });

      // Check failed commit reads before inspecting the base references they must not export.
      if (scenario.check) {
        scenario.check(result);
        return;
      }

      // Successful decisions must complete even though the Git stub rejects push lookups.
      assert.equal(result.status, 0, result.stdout + result.stderr);

      // Verify all six producer decisions and the references consumed by downstream builds.
      const outputs = Object.fromEntries(
        result.output.trim().split("\n").map((line) => line.split("=")),
      );
      const selectedRegistry = scenario.rebuild ? targetRegistry : sourceRegistry;
      const selectedVersion = scenario.rebuild ? imageVersion : "latest";
      for (const image of baseImages) {
        const outputPrefix = image.replaceAll("-", "_");
        assert.equal(outputs[outputPrefix + "_run"], String(scenario.rebuild), image);
        assert.equal(outputs[outputPrefix + "_version"], selectedVersion, image);
        assert.equal(
          outputs[outputPrefix + "_ref"],
          `${selectedRegistry}/${image}:${selectedVersion}`,
          image,
        );
      }
    });
  }
});

/**
 * Suppress shared aliases only for PR publishing so ordinary PR base rebuilds
 * and main/tag publication keep their existing tags.
 */
test("image tag preparation isolates only PR publishing", async (t) => {
  const image = "registry.example/dev/image";
  const cases = [
    // PR publishing must suppress both the default latest tag and additional aliases.
    { name: "PR publishing", prPublish: "true", tagLatest: "true", expected: [image + ":version"] },
    // Ordinary PR base rebuilds must retain their existing version-series alias.
    { name: "ordinary PR", prPublish: "false", tagLatest: "false", expected: [image + ":version", image + ":v2.0-latest"] },
    // Main/tag publication must retain both aliases used by existing callers.
    { name: "main or tag", prPublish: "false", tagLatest: "true", expected: [image + ":version", image + ":latest", image + ":v2.0-latest"] },
  ];
  for (const scenario of cases) {
    await t.test(scenario.name, (t) => {
      // Supply the existing tag inputs for each publication mode.
      const result = runWorkflowScript(t, {
        workflow: "docker-build.yml", section: "      - name: Prepare tag list",
        field: "run", indent: 10,
        environment: {
          IMAGE_NAME: image, IMAGE_TAG: "version", TAG_LATEST: scenario.tagLatest,
          ADDITIONAL_TAGS: image + ":v2.0-latest",
          PR_PUBLISH: scenario.prPublish, // Suppress aliases only for the new command.
        },
      });

      // Compare the tags actually written for Buildx by the shipped workflow.
      assert.equal(result.status, 0, result.stderr);
      assert.deepEqual(result.output.match(/tags<<EOF\n([\s\S]*?)\nEOF/)[1].split("\n"), scenario.expected);
    });
  }
});

/**
 * Publish each PR manifest only under its version and propagate failed PR pushes
 * so shared aliases remain protected and registry failures cannot pass PR publishing.
 * Preserve main/tag failure handling to avoid changing existing publication behavior.
 */
test("manifest jobs isolate PR versions and fail on registry errors", async (t) => {
  const jobs = [
    // Core combines digest-addressed images before updating its version-series alias.
    { name: "merge-manifests-nvmetal-carbide", releaseFailureStatus: 0, releasePushes: 2 },
    // CLI additionally maintains a bare latest alias that PRs must also protect.
    { name: "merge-manifests-forge-cli", releaseFailureStatus: 0, releasePushes: 3 },
    // The basic plugin publishes a separate multi-architecture manifest.
    { name: "merge-manifests-machine-validation-basic-plugin", releaseFailureStatus: 1, releasePushes: 2 },
    // Machine validation publishes the configuration images under one combined manifest.
    { name: "merge-manifests-machine-validation", releaseFailureStatus: 0, releasePushes: 2 },
    // Lifecycle tests publish their own versioned manifest when those builds are selected.
    { name: "merge-manifests-machine-lifecycle-test", releaseFailureStatus: 1, releasePushes: 2 },
  ];
  for (const job of jobs) {
    await t.test(job.name, (t) => {
      // Mock only Docker's external effect; execute each job's actual command.
      const options = {
        workflow: "ci.yaml", section: "  " + job.name + ":",
        field: "command", indent: 12,
        environment: {
          IMAGE_REGISTRY: "registry.example/dev", VERSION: "v2.0.0-pr.123.aaaaaaa",
          MAJOR_MINOR_VERSION: "v2.0", X86_DIGEST: "sha256:amd64", ARM_DIGEST: "sha256:arm64",
          PUBLISH_RELEASE: "false", // Permit the PR version while protecting shared aliases.
          FAIL_PUSH: "false", FAIL_FIRST_PUSH: "false",
        },
        prelude: `
          docker() {
            printf '%s\\n' "$*" >> "$OUTPUT"
            # Fail the version push while allowing later aliases to preserve release behavior.
            if [[ "\${FAIL_FIRST_PUSH:-false}" == "true" && $(wc -l < "$OUTPUT") -eq 1 ]]; then
              return 1
            fi
            [[ "\${FAIL_PUSH:-false}" != "true" ]]
          }
        `,
      };

      // A PR run must create its versioned manifest without touching any latest alias.
      const pr = runWorkflowScript(t, options);
      assert.equal(pr.status, 0, pr.stderr);
      assert.equal(pr.output.trim().split("\n").length, 1);
      assert.ok(pr.output.includes(":v2.0.0-pr.123.aaaaaaa"));
      assert.ok(!pr.output.includes("latest"));

      // Release mode must preserve all existing aliases for this image.
      const release = runWorkflowScript(t, {
        ...options, environment: { ...options.environment, PUBLISH_RELEASE: "true" },
      });
      assert.equal(release.status, 0, release.stderr);
      assert.equal(release.output.trim().split("\n").length, job.releasePushes);

      // Fail the version push to exercise each job's existing release behavior.
      const releaseFailure = runWorkflowScript(t, {
        ...options,
        environment: { ...options.environment, PUBLISH_RELEASE: "true", FAIL_FIRST_PUSH: "true" },
      });

      // Preserve its exit status and whether later aliases are attempted.
      assert.equal(releaseFailure.status, job.releaseFailureStatus, releaseFailure.stderr);
      assert.equal(
        releaseFailure.output.trim().split("\n").length,
        job.releaseFailureStatus === 0 ? job.releasePushes : 1,
      );

      // Fail the primary push to prove a skipped alias cannot hide the registry error.
      const failure = runWorkflowScript(t, {
        ...options, environment: { ...options.environment, FAIL_PUSH: "true" },
      });
      assert.equal(failure.status, 1, failure.stderr);
    });
  }
});
