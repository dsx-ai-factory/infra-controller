// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dao

import (
	"fmt"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	eventrulecodec "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/codec"
)

// EventActionExecutionTo converts a domain execution to a database model.
func EventActionExecutionTo(
	execution *eventrule.Execution,
) (*dbmodel.EventActionExecution, error) {
	if err := execution.Validate(); err != nil {
		return nil, err
	}

	plan, err := eventrulecodec.MarshalExecutionPlan(execution.Plan)
	if err != nil {
		return nil, fmt.Errorf("encode execution plan: %w", err)
	}

	return &dbmodel.EventActionExecution{
		ID:             execution.ID,
		EventID:        execution.EventID,
		ActionName:     execution.ActionName,
		ActionType:     string(execution.Plan.Type()),
		Plan:           plan,
		Status:         string(execution.Status),
		Reason:         cutil.GetPtrIfNotZero(string(execution.Reason)),
		Attempts:       execution.Attempts,
		ClaimToken:     cutil.GetPtrIfNotZero(execution.ClaimToken),
		ClaimOwner:     cutil.GetPtrIfNotZero(execution.ClaimOwner),
		ClaimExpiresAt: cutil.GetPtrIfNotZero(execution.ClaimExpiresAt),
		StatusMessage:  cutil.GetPtrIfNotZero(execution.StatusMessage),
		CreatedAt:      execution.CreatedAt,
		UpdatedAt:      execution.UpdatedAt,
		NextAttemptAt:  cutil.GetPtrIfNotZero(execution.NextAttemptAt),
	}, nil
}

// EventActionExecutionFrom converts a database model to a domain execution.
func EventActionExecutionFrom(
	persisted *dbmodel.EventActionExecution,
) (*eventrule.Execution, error) {
	if persisted == nil {
		return nil, nil
	}

	plan, err := eventrulecodec.UnmarshalExecutionPlan(persisted.Plan)
	if err != nil {
		return nil, fmt.Errorf(
			"%w: decode plan: %w",
			eventrule.ErrInvalidPersistedExecution,
			err,
		)
	}

	if string(plan.Type()) != persisted.ActionType {
		return nil, fmt.Errorf(
			"%w: action type %q does not match plan type %q",
			eventrule.ErrInvalidPersistedExecution,
			persisted.ActionType,
			plan.Type(),
		)
	}

	execution := &eventrule.Execution{
		ExecutionState: eventrule.ExecutionState{
			ExecutionStatusDetails: eventrule.ExecutionStatusDetails{
				Status:        eventrule.ExecutionStatus(persisted.Status),
				Reason:        eventrule.ExecutionReason(cutil.GetValueOrZero(persisted.Reason)),
				StatusMessage: cutil.GetValueOrZero(persisted.StatusMessage),
			},
			NextAttemptAt: optionalTimeFromPersistence(persisted.NextAttemptAt),
		},
		ID:         persisted.ID,
		EventID:    persisted.EventID,
		ActionName: persisted.ActionName,
		Plan:       plan,
		Attempts:   persisted.Attempts,
		ClaimToken: cutil.GetValueOrZero(persisted.ClaimToken),
		ClaimOwner: cutil.GetValueOrZero(persisted.ClaimOwner),
		ClaimExpiresAt: optionalTimeFromPersistence(
			persisted.ClaimExpiresAt,
		),
		CreatedAt: timeFromPersistence(persisted.CreatedAt),
		UpdatedAt: timeFromPersistence(persisted.UpdatedAt),
	}

	if err := execution.Validate(); err != nil {
		return nil, fmt.Errorf("%w: %w", eventrule.ErrInvalidPersistedExecution, err)
	}

	return execution, nil
}
