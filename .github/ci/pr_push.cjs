// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

/**
 * Authorize the current PR commit and matching bot mirror so publishing cannot
 * reuse another PR's comment or follow a commit that the request did not approve.
 * Callers dispatching a build must pass the returned SHA as expectedSha when
 * validating that build before it runs.
 */
async function authorizePush({ github, owner, repo, prNumber, commentId, expectedSha }) {
  if (!Number.isSafeInteger(prNumber) || prNumber <= 0 ||
      !Number.isSafeInteger(commentId) || commentId <= 0) {
    throw new Error("PR and comment IDs must be positive integers");
  }

  // Read the original comment again so dispatch inputs cannot supply authorization.
  const { data: comment } = await github.rest.issues.getComment({
    owner, repo, comment_id: commentId,
  });
  if (new URL(comment.issue_url).pathname !== "/repos/" + owner + "/" + repo + "/issues/" + prNumber) {
    throw new Error("The publishing comment belongs to another PR");
  }
  const command = comment.body.trim().match(/^\/ok to push(?: ([0-9a-f]{40}))?$/);
  if (!command) {
    throw new Error("Use /ok to push or /ok to push <full lowercase commit SHA>");
  }

  // GitHub maps Maintain to write in this API's legacy permission field.
  const { data: permission } = await github.rest.repos.getCollaboratorPermissionLevel({
    owner, repo, username: comment.user.login,
  });
  if (!["write", "admin"].includes(permission.permission)) {
    throw new Error("PR image publishing requires Write, Maintain, or Admin access");
  }

  // Bind bare commands to the current head and reject stale explicit approvals.
  const { data: pr } = await github.rest.pulls.get({ owner, repo, pull_number: prNumber });
  if (pr.state !== "open" || pr.base.repo.full_name !== owner + "/" + repo) {
    throw new Error("PR image publishing requires an open PR in this repository");
  }
  const sha = command[1] || pr.head.sha;
  if (sha !== pr.head.sha || (expectedSha !== undefined && sha !== expectedSha)) {
    throw new Error("The PR head changed; request /ok to push again for the current SHA");
  }

  // Require copy-pr-bot admission before executing code on the existing CI runners.
  const ref = "pull-request/" + prNumber;
  let mirror;
  try {
    ({ data: mirror } = await github.rest.git.getRef({ owner, repo, ref: "heads/" + ref }));
  } catch (error) {
    if (error.status !== 404) throw error;
    throw new Error("The PR mirror is missing; request /ok to test " + sha + " first");
  }
  if (mirror.object.sha !== sha) {
    throw new Error("The PR mirror is stale; request /ok to test " + sha + " first");
  }
  return { ref, sha };
}

module.exports = { authorizePush };
