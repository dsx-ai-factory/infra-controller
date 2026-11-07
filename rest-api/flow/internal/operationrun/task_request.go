// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrun

import (
	"fmt"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
)

const targetIdempotencyKeyPrefix = "operation-run-target:"

// TaskRequestForTarget specializes an operation template to one planned rack
// target. The same request shape is used for admission and later dispatch.
func TaskRequestForTarget(
	op *Operation,
	target *OperationRunTarget,
) (*operation.Request, error) {
	if op == nil {
		return nil, fmt.Errorf("operation is required")
	}
	if target == nil {
		return nil, fmt.Errorf("operation run target is required")
	}

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
		IdempotencyKey:   TargetIdempotencyKey(target.ID),
	}, nil
}

// TargetIdempotencyKey returns the stable child-task key for a planned target.
func TargetIdempotencyKey(targetID uuid.UUID) string {
	return targetIdempotencyKeyPrefix + targetID.String()
}
