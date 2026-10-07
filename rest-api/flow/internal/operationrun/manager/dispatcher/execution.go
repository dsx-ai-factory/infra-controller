// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dispatcher

import (
	"context"
	"fmt"
	"time"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
)

const idempotencyKeyPrefix = "operation-run-target:"

func (d *Dispatcher) execute(
	prep *preparedDispatch,
	decision dispatchDecision,
	now time.Time,
) (claimedTargets, error) {
	switch decision.action {
	case dispatchRunActionStop:
		decision.transition.apply(prep.run, now)
		return claimedTargets{}, nil
	case dispatchRunActionClaim:
		decision.transition.apply(prep.run, now)
		claimed, err := claim(prep, decision, now, d.cfg.ClaimLease)
		if err != nil {
			prep.run.Fail(now, err.Error())
			return claimedTargets{}, nil
		}
		return claimed, nil
	default:
		return claimedTargets{}, fmt.Errorf("unsupported dispatch decision action %d", decision.action)
	}
}

// claim selects ready targets in the current phase, leases them for
// post-commit submission, and returns the work to submit after commit.
func claim(
	prep *preparedDispatch,
	decision dispatchDecision,
	now time.Time,
	claimLease time.Duration,
) (claimedTargets, error) {
	summary := summarizeClaimTargets(decision.targets, now)
	available := int(decision.options.MaxConcurrentTargets) - summary.active
	if available <= 0 {
		prep.run.StatusMessage = "waiting for active targets quota"
		return claimedTargets{}, nil
	}

	claimed := claimedTargets{
		targets:        make([]*operationrun.OperationRunTarget, 0, available),
		requests:       make([]*operation.Request, 0, available),
		conflictPolicy: decision.conflictPolicy,
	}

	for _, target := range summary.candidates {
		req, err := operationRequestForTarget(decision.op, target)
		if err != nil {
			target.Fail(err.Error())
			prep.changed[target.ID] = target
			continue
		}

		leaseExpiresAt := now.Add(claimLease)
		target.Claim(leaseExpiresAt, "claimed for submission")
		prep.changed[target.ID] = target

		claimed.add(target, req)
		if claimed.len() >= available {
			break
		}
	}

	if msg := claimStatusMessage(
		claimed.len(),
		summary.blocked,
		decision.conflictPolicy,
	); msg != "" {
		prep.run.StatusMessage = msg
	}

	return claimed, nil
}

type claimTargetSummary struct {
	active     int
	blocked    int
	candidates []*operationrun.OperationRunTarget
}

func summarizeClaimTargets(
	targets []*operationrun.OperationRunTarget,
	now time.Time,
) claimTargetSummary {
	summary := claimTargetSummary{
		candidates: make([]*operationrun.OperationRunTarget, 0, len(targets)),
	}

	for _, target := range targets {
		if targetConsumesQuota(target, now) {
			summary.active++
		}

		if targetReadyForSubmission(target, now) {
			summary.candidates = append(summary.candidates, target)
			continue
		}

		if target.Status == operationrun.OperationRunTargetStatusBlocked {
			summary.blocked++
		}
	}

	return summary
}

func targetConsumesQuota(
	target *operationrun.OperationRunTarget,
	now time.Time,
) bool {
	switch target.Status {
	case operationrun.OperationRunTargetStatusSubmitted:
		return target.TaskID != nil
	case operationrun.OperationRunTargetStatusClaimed:
		return target.RetryAfter != nil && target.RetryAfter.After(now)
	default:
		return false
	}
}

func claimStatusMessage(
	submitted int,
	blocked int,
	conflictPolicy conflictPolicyRuntime,
) string {
	submittedMsg := submittedStatusMessage(submitted)
	blockedMsg := waitingTargetsStatusMessage(blocked, conflictPolicy)
	if submittedMsg != "" && blockedMsg != "" {
		return fmt.Sprintf("%s; %s", submittedMsg, blockedMsg)
	}
	if submittedMsg != "" {
		return submittedMsg
	}
	return blockedMsg
}

func submittedStatusMessage(count int) string {
	switch count {
	case 0:
		return ""
	case 1:
		return "submitted 1 operation run target"
	default:
		return fmt.Sprintf("submitted %d operation run targets", count)
	}
}

func waitingTargetsStatusMessage(
	count int,
	conflictPolicy conflictPolicyRuntime,
) string {
	switch count {
	case 0:
		return ""
	case 1:
		return fmt.Sprintf("1 target %s", conflictPolicy.waitingStatusMessage())
	default:
		return fmt.Sprintf("%d targets %s", count, conflictPolicy.waitingStatusMessage())
	}
}

// claimedTargets carries the claimed targets, their corresponding task
// requests, and the run-level conflict policy across the transaction boundary.
type claimedTargets struct {
	targets        []*operationrun.OperationRunTarget
	requests       []*operation.Request
	conflictPolicy conflictPolicyRuntime
}

func (c *claimedTargets) add(
	target *operationrun.OperationRunTarget,
	req *operation.Request,
) {
	c.targets = append(c.targets, target)
	c.requests = append(c.requests, req)
}

func (c claimedTargets) len() int {
	return len(c.targets)
}

// submit creates the child task for a previously claimed target and records
// either the task ID, a retryable rack conflict, or a hard failure.
func (d *Dispatcher) submit(
	ctx context.Context,
	conflictPolicy conflictPolicyRuntime,
	target *operationrun.OperationRunTarget,
	req *operation.Request,
	now time.Time,
) (bool, error) {
	taskIDs, err := d.deps.TaskManager.SubmitTask(ctx, req)
	if err != nil {
		if conflictPolicy.handleSubmissionError(target, err, now) {
			return false, d.updateTargetAfterSubmit(ctx, target)
		}

		target.Fail(err.Error())
		return false, d.updateTargetAfterSubmit(ctx, target)
	}

	if len(taskIDs) != 1 || taskIDs[0] == uuid.Nil {
		target.Fail("task submission did not return exactly one task")
		return false, d.updateTargetAfterSubmit(ctx, target)
	}

	taskID := taskIDs[0]
	target.Submit(taskID, "submitted")
	return true, d.updateTargetAfterSubmit(ctx, target)
}

func (d *Dispatcher) updateTargetAfterSubmit(
	ctx context.Context,
	target *operationrun.OperationRunTarget,
) error {
	persistCtx, cancel := context.WithTimeout(
		context.WithoutCancel(ctx),
		d.cfg.SubmitPersistTimeout,
	)
	defer cancel()

	return d.deps.Store.UpdateTargetState(persistCtx, target)
}

// operationRequestForTarget specializes the operation template to one target
// rack and its frozen component set.
// TODO: Introduce a common operation request builder for shared operation
// wrapper, description, conflict policy, rule ID, and rack constraint handling.
func operationRequestForTarget(
	op *operationrun.Operation,
	target *operationrun.OperationRunTarget,
) (*operation.Request, error) {
	info, err := op.Payload.Marshal()
	if err != nil {
		return nil, fmt.Errorf("marshal operation payload: %w", err)
	}
	ruleID, err := operations.ExtractRuleID(info)
	if err != nil {
		return nil, fmt.Errorf("extract operation rule ID: %w", err)
	}

	componentIDs := target.ComponentsByType.AllComponentUUIDs()
	components := make([]operation.ComponentTarget, 0, len(componentIDs))
	for _, id := range componentIDs {
		components = append(components, operation.ComponentTarget{UUID: id})
	}

	description := op.Description
	if description == "" {
		description = op.Payload.Description()
	}

	return &operation.Request{
		Operation: operation.Wrapper{
			Type: op.Payload.Type(),
			Code: op.Payload.CodeString(),
			Info: info,
		},
		TargetSpec: operation.TargetSpec{
			Components: components,
		},
		Description:      description,
		ConflictStrategy: operation.ConflictStrategyReject,
		RuleID:           ruleID,
		RequiredRackID:   target.RackID,
		IdempotencyKey:   targetIdempotencyKey(target.ID),
	}, nil
}

func targetIdempotencyKey(targetID uuid.UUID) string {
	return idempotencyKeyPrefix + targetID.String()
}

// targetReadyForSubmission identifies targets that can be claimed now,
// including blocked targets whose conflict retry delay has expired and claimed
// targets whose claim lease has expired.
func targetReadyForSubmission(
	target *operationrun.OperationRunTarget,
	now time.Time,
) bool {
	switch target.Status {
	case operationrun.OperationRunTargetStatusPending:
		return true
	case operationrun.OperationRunTargetStatusBlocked,
		operationrun.OperationRunTargetStatusClaimed:
		return target.RetryAfter == nil || !target.RetryAfter.After(now)
	case operationrun.OperationRunTargetStatusSubmitted:
		return target.TaskID == nil
	default:
		return false
	}
}
