// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package codec

import (
	"encoding/json"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	flowtypes "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

const actionVersionV1 = 1

type actionV1 struct {
	Version   int               `json:"version"`
	Name      string            `json:"name"`
	Type      string            `json:"type"`
	Condition actionConditionV1 `json:"condition"`
	Spec      json.RawMessage   `json:"spec"`
}

type actionConditionV1 struct {
	Severities     []string `json:"severities,omitempty"`
	ComponentTypes []string `json:"componentTypes,omitempty"`
}

type submitTaskSpecV1 struct {
	Operation        taskOperationV1 `json:"operation"`
	TargetStrategy   string          `json:"targetStrategy"`
	ConflictStrategy string          `json:"conflictStrategy"`
	Description      string          `json:"description,omitempty"`
}

type taskOperationV1 struct {
	Type    string          `json:"type"`
	Code    string          `json:"code"`
	Payload json.RawMessage `json:"payload"`
}

type sendAlertSpecV1 struct {
	Severity string `json:"severity"`
	Message  string `json:"message,omitempty"`
}

type noopSpecV1 struct {
	Reason string `json:"reason,omitempty"`
}

func marshalActionV1(action eventrule.Action) (json.RawMessage, error) {
	persisted := actionV1{
		Version:   actionVersionV1,
		Name:      action.Name,
		Type:      string(action.Spec.Type()),
		Condition: actionConditionToV1(action.Condition),
	}

	spec, err := marshalActionSpecV1(action.Spec)
	if err != nil {
		return nil, err
	}
	persisted.Spec = spec

	encoded, err := json.Marshal(persisted)
	if err != nil {
		return nil, fmt.Errorf("encode event policy action v1: %w", err)
	}

	return encoded, nil
}

func unmarshalActionV1(data json.RawMessage) (eventrule.Action, error) {
	var persisted actionV1
	if err := decodeStrict(data, &persisted); err != nil {
		return eventrule.Action{}, fmt.Errorf("decode event policy action v1: %w", err)
	}

	condition, err := actionConditionFromV1(persisted.Condition)
	if err != nil {
		return eventrule.Action{}, err
	}

	spec, err := unmarshalActionSpecV1(eventrule.ActionType(persisted.Type), persisted.Spec)
	if err != nil {
		return eventrule.Action{}, err
	}

	return eventrule.Action{
		Name:      persisted.Name,
		Condition: condition,
		Spec:      spec,
	}, nil
}

func actionConditionToV1(condition eventrule.ActionCondition) actionConditionV1 {
	persisted := actionConditionV1{}
	if condition.Severities != nil {
		persisted.Severities = make([]string, len(condition.Severities))
		for i, severity := range condition.Severities {
			persisted.Severities[i] = string(severity)
		}
	}
	if condition.ComponentTypes != nil {
		persisted.ComponentTypes = make([]string, len(condition.ComponentTypes))
		for i, componentType := range condition.ComponentTypes {
			persisted.ComponentTypes[i] = string(componentType)
		}
	}
	return persisted
}

func actionConditionFromV1(
	persisted actionConditionV1,
) (eventrule.ActionCondition, error) {
	condition := eventrule.ActionCondition{}
	if persisted.Severities != nil {
		condition.Severities = make([]eventrule.Severity, len(persisted.Severities))
		for i, severity := range persisted.Severities {
			decodedSeverity, err := eventrule.ParseSeverity(severity)
			if err != nil {
				return eventrule.ActionCondition{}, fmt.Errorf(
					"condition severities[%d]: %w",
					i,
					err,
				)
			}
			condition.Severities[i] = decodedSeverity
		}
	}

	if persisted.ComponentTypes != nil {
		condition.ComponentTypes = make([]flowtypes.ComponentType, len(persisted.ComponentTypes))
		for i, componentType := range persisted.ComponentTypes {
			decodedComponentType, err := flowtypes.ParseComponentType(componentType)
			if err != nil {
				return eventrule.ActionCondition{}, fmt.Errorf(
					"condition componentTypes[%d]: %w",
					i,
					err,
				)
			}
			condition.ComponentTypes[i] = decodedComponentType
		}
	}
	return condition, nil
}

func marshalActionSpecV1(spec eventrule.ActionSpec) (json.RawMessage, error) {
	switch typed := spec.(type) {
	case *eventrule.SubmitTask:
		if typed == nil {
			return nil, fmt.Errorf("submit_task spec is required")
		}
		if typed.Operation == nil {
			return nil, fmt.Errorf("submit_task operation is required")
		}
		operationPayload, err := typed.Operation.Marshal()
		if err != nil {
			return nil, fmt.Errorf("encode submit_task operation v1: %w", err)
		}

		return json.Marshal(submitTaskSpecV1{
			Operation: taskOperationV1{
				Type:    typed.Operation.Type().String(),
				Code:    typed.Operation.CodeString(),
				Payload: operationPayload,
			},
			TargetStrategy:   string(typed.TargetStrategy),
			ConflictStrategy: string(typed.ConflictStrategy),
			Description:      typed.Description,
		})
	case *eventrule.SendAlert:
		if typed == nil {
			return nil, fmt.Errorf("send_alert spec is required")
		}
		return json.Marshal(sendAlertSpecV1{
			Severity: string(typed.Severity),
			Message:  typed.Message,
		})
	case *eventrule.Noop:
		if typed == nil {
			return nil, fmt.Errorf("noop spec is required")
		}
		return json.Marshal(noopSpecV1{Reason: typed.Reason})
	default:
		return nil, fmt.Errorf("unsupported action spec %T", spec)
	}
}

func unmarshalActionSpecV1(
	actionType eventrule.ActionType,
	data json.RawMessage,
) (eventrule.ActionSpec, error) {
	switch actionType {
	case eventrule.ActionTypeSubmitTask:
		var persisted submitTaskSpecV1
		if err := decodeStrict(data, &persisted); err != nil {
			return nil, fmt.Errorf("decode submit_task action spec v1: %w", err)
		}
		operationType := taskcommon.TaskType(persisted.Operation.Type)
		operationCode := taskcommon.OperationCode(persisted.Operation.Code)
		if err := operationCode.ValidateFor(operationType); err != nil {
			return nil, fmt.Errorf("decode submit_task operation v1: %w", err)
		}
		operation, err := operations.New(operationType, persisted.Operation.Payload)
		if err != nil {
			return nil, fmt.Errorf("decode submit_task operation payload v1: %w", err)
		}
		if err := operation.Validate(); err != nil {
			return nil, fmt.Errorf("validate submit_task operation payload v1: %w", err)
		}
		if operation.CodeString() != operationCode.String() {
			return nil, fmt.Errorf(
				"submit_task operation code %q does not match payload code %q",
				operationCode,
				operation.CodeString(),
			)
		}

		return &eventrule.SubmitTask{
			Operation:        operation,
			TargetStrategy:   eventrule.TargetStrategy(persisted.TargetStrategy),
			ConflictStrategy: eventrule.ConflictStrategy(persisted.ConflictStrategy),
			Description:      persisted.Description,
		}, nil
	case eventrule.ActionTypeSendAlert:
		var persisted sendAlertSpecV1
		if err := decodeStrict(data, &persisted); err != nil {
			return nil, fmt.Errorf("decode send_alert action spec v1: %w", err)
		}

		severity, err := eventrule.ParseSeverity(persisted.Severity)
		if err != nil {
			return nil, fmt.Errorf("decode send_alert action spec v1 severity: %w", err)
		}
		return &eventrule.SendAlert{
			Severity: severity,
			Message:  persisted.Message,
		}, nil
	case eventrule.ActionTypeNoop:
		var persisted noopSpecV1
		if err := decodeStrict(data, &persisted); err != nil {
			return nil, fmt.Errorf("decode noop action spec v1: %w", err)
		}

		return &eventrule.Noop{Reason: persisted.Reason}, nil
	default:
		return nil, fmt.Errorf("unknown action type %q", actionType)
	}
}
