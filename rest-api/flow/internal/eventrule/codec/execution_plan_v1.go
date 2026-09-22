// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package codec

import (
	"encoding/json"
	"fmt"
	"slices"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

const executionPlanVersionV1 = 1

type executionPlanV1 struct {
	Version int             `json:"version"`
	Type    string          `json:"type"`
	Plan    json.RawMessage `json:"plan"`
}

type submitTaskPlan struct {
	Operation        taskOperation `json:"operation"`
	Description      string        `json:"description,omitempty"`
	ConflictStrategy string        `json:"conflictStrategy"`
	Targets          []rackTarget  `json:"targets"`
}

func (p submitTaskPlan) executionPlan() (eventrule.ExecutionPlan, error) {
	targets, err := targetsFromV1(p.Targets)
	if err != nil {
		return nil, err
	}

	conflictStrategy, err := operation.ParseConflictStrategy(p.ConflictStrategy)
	if err != nil {
		return nil, err
	}

	return &eventrule.SubmitTaskPlan{
		Operation: operation.Wrapper{
			Type: taskcommon.TaskType(p.Operation.Type),
			Code: p.Operation.Code,
			Info: append(json.RawMessage(nil), p.Operation.Info...),
		},
		Description:      p.Description,
		ConflictStrategy: conflictStrategy,
		Targets:          targets,
	}, nil
}

type taskOperation struct {
	Type string          `json:"type"`
	Code string          `json:"code"`
	Info json.RawMessage `json:"info"`
}

type rackTarget struct {
	RackID     uuid.UUID        `json:"rackId"`
	Components []componentGroup `json:"components"`
}

type componentGroup struct {
	Type string      `json:"type"`
	IDs  []uuid.UUID `json:"ids"`
}

type sendAlertPlan struct {
	Severity string `json:"severity"`
	Message  string `json:"message,omitempty"`
}

func (p sendAlertPlan) executionPlan() (eventrule.ExecutionPlan, error) {
	return &eventrule.SendAlertPlan{
		Severity: eventrule.Severity(p.Severity),
		Message:  p.Message,
	}, nil
}

type noopPlan struct {
	Reason string `json:"reason,omitempty"`
}

func (p noopPlan) executionPlan() (eventrule.ExecutionPlan, error) {
	return &eventrule.NoopPlan{Reason: p.Reason}, nil
}

func marshalExecutionPlanV1(plan eventrule.ExecutionPlan) (json.RawMessage, error) {
	persistedPlan, err := persistedPlanV1(plan)
	if err != nil {
		return nil, err
	}

	persisted, err := json.Marshal(persistedPlan)
	if err != nil {
		return nil, fmt.Errorf("encode %s execution plan v1: %w", plan.Type(), err)
	}

	v1 := executionPlanV1{
		Version: executionPlanVersionV1,
		Type:    string(plan.Type()),
		Plan:    persisted,
	}

	encoded, err := json.Marshal(v1)
	if err != nil {
		return nil, fmt.Errorf("encode execution plan v1: %w", err)
	}

	return encoded, nil
}

func unmarshalExecutionPlanV1(data json.RawMessage) (eventrule.ExecutionPlan, error) {
	var v1 executionPlanV1
	if err := decodeStrict(data, &v1); err != nil {
		return nil, fmt.Errorf("decode execution plan v1: %w", err)
	}

	plan, err := planFromV1(v1)
	if err != nil {
		return nil, fmt.Errorf("decode %s execution plan v1: %w", v1.Type, err)
	}

	return plan, nil
}

func persistedPlanV1(plan eventrule.ExecutionPlan) (any, error) {
	switch typed := plan.(type) {
	case *eventrule.SubmitTaskPlan:
		return submitTaskPlan{
			Operation: taskOperation{
				Type: typed.Operation.Type.String(),
				Code: typed.Operation.Code,
				Info: append(json.RawMessage(nil), typed.Operation.Info...),
			},
			Description:      typed.Description,
			ConflictStrategy: typed.ConflictStrategy.String(),
			Targets:          targetsV1(typed.Targets),
		}, nil
	case *eventrule.SendAlertPlan:
		return sendAlertPlan{
			Severity: string(typed.Severity),
			Message:  typed.Message,
		}, nil
	case *eventrule.NoopPlan:
		return noopPlan{Reason: typed.Reason}, nil
	default:
		return nil, fmt.Errorf("unsupported execution plan %T", plan)
	}
}

func planFromV1(v1 executionPlanV1) (eventrule.ExecutionPlan, error) {
	switch eventrule.ActionType(v1.Type) {
	case eventrule.ActionTypeSubmitTask:
		var persisted submitTaskPlan
		if err := decodeStrict(v1.Plan, &persisted); err != nil {
			return nil, err
		}

		return persisted.executionPlan()
	case eventrule.ActionTypeSendAlert:
		var persisted sendAlertPlan
		if err := decodeStrict(v1.Plan, &persisted); err != nil {
			return nil, err
		}

		return persisted.executionPlan()
	case eventrule.ActionTypeNoop:
		var persisted noopPlan
		if err := decodeStrict(v1.Plan, &persisted); err != nil {
			return nil, err
		}

		return persisted.executionPlan()
	default:
		return nil, fmt.Errorf("unknown execution plan type %q", v1.Type)
	}
}

func targetsV1(src []operation.RackExecutionTarget) []rackTarget {
	dst := make([]rackTarget, len(src))
	for i, t := range src {
		groups := make([]componentGroup, 0, len(t.ComponentsByType))
		for _, ct := range t.ComponentsByType.SortedComponentTypes() {
			g := componentGroup{
				Type: devicetypes.ComponentTypeToString(ct),
				IDs:  slices.Clone(t.ComponentsByType[ct]),
			}

			groups = append(groups, g)
		}

		dst[i] = rackTarget{RackID: t.RackID, Components: groups}
	}

	return dst
}

func targetsFromV1(src []rackTarget) ([]operation.RackExecutionTarget, error) {
	dst := make([]operation.RackExecutionTarget, len(src))
	for i, t := range src {
		byType := make(operation.ComponentsByType, len(t.Components))
		for _, g := range t.Components {
			ct := devicetypes.ComponentTypeFromString(g.Type)
			if ct == devicetypes.ComponentTypeUnknown {
				return nil, fmt.Errorf("unknown component type %q", g.Type)
			}

			if _, ok := byType[ct]; ok {
				return nil, fmt.Errorf("duplicate component type %q", g.Type)
			}

			byType[ct] = slices.Clone(g.IDs)
		}

		dst[i] = operation.RackExecutionTarget{
			RackID:           t.RackID,
			ComponentsByType: byType,
		}
	}

	return dst, nil
}
