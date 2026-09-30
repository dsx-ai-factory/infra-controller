// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package protobuf

import (
	"fmt"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
	flowtypes "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	"github.com/google/uuid"
	"google.golang.org/protobuf/types/known/timestamppb"
)

// EventRuleCreateFrom converts and validates the boundary representation of a
// persisted event rule. Runtime compatibility is validated by the manager.
func EventRuleCreateFrom(req *pb.CreateEventRuleRequest) (eventrule.RuleCreate, error) {
	if req == nil {
		return eventrule.RuleCreate{}, fmt.Errorf("event rule request is required")
	}

	actions, err := EventRuleActionsFrom(req.GetActions())
	if err != nil {
		return eventrule.RuleCreate{}, err
	}

	input := eventrule.RuleCreate{
		Metadata: eventrule.RuleMetadata{
			Name:        req.GetName(),
			Description: req.GetDescription(),
		},
		EventType: eventrule.Type(req.GetEventType()),
		Policy:    eventrule.Policy{Actions: actions},
	}
	if err := input.Validate(); err != nil {
		return eventrule.RuleCreate{}, err
	}

	return input, nil
}

// EventRuleFilterFrom converts optional list filters.
func EventRuleFilterFrom(req *pb.ListEventRulesRequest) (eventrule.RuleFilter, error) {
	filter := eventrule.RuleFilter{}
	if req == nil {
		return filter, nil
	}

	if req.EventType != nil {
		eventType := eventrule.Type(req.GetEventType())
		if err := eventType.Validate(); err != nil {
			return eventrule.RuleFilter{}, err
		}
		filter.EventType = &eventType
	}
	if req.Enabled != nil {
		enabled := req.GetEnabled()
		filter.Enabled = &enabled
	}

	return filter, nil
}

// EventRuleListRequestFrom converts filters and validated offset pagination.
func EventRuleListRequestFrom(
	req *pb.ListEventRulesRequest,
) (eventrule.RuleListRequest, error) {
	filter, err := EventRuleFilterFrom(req)
	if err != nil {
		return eventrule.RuleListRequest{}, err
	}

	var pagination *pb.Pagination
	if req != nil {
		pagination = req.GetPagination()
	}
	convertedPagination := PaginationFrom(pagination)
	request := eventrule.RuleListRequest{
		Filter: filter,
		Offset: convertedPagination.Offset,
		Limit:  convertedPagination.Limit,
	}
	if err := request.Validate(); err != nil {
		return eventrule.RuleListRequest{}, err
	}

	return request, nil
}

// EventRuleActionsFrom converts typed action specifications.
func EventRuleActionsFrom(actions []*pb.EventRuleAction) ([]eventrule.Action, error) {
	if actions == nil {
		return nil, nil
	}

	converted := make([]eventrule.Action, len(actions))
	for i, action := range actions {
		item, err := eventRuleActionFrom(action)
		if err != nil {
			return nil, fmt.Errorf("actions[%d]: %w", i, err)
		}
		converted[i] = item
	}

	if err := eventrule.ValidateActions(converted); err != nil {
		return nil, err
	}

	return converted, nil
}

func eventRuleActionFrom(action *pb.EventRuleAction) (eventrule.Action, error) {
	if action == nil {
		return eventrule.Action{}, fmt.Errorf("action is required")
	}

	condition, err := eventRuleActionConditionFrom(action.GetCondition())
	if err != nil {
		return eventrule.Action{}, err
	}

	converted := eventrule.Action{Name: action.GetName(), Condition: condition}
	switch spec := action.GetSpec().(type) {
	case *pb.EventRuleAction_SubmitTask:
		converted.Spec, err = eventRuleSubmitTaskFrom(spec.SubmitTask)
	case *pb.EventRuleAction_SendAlert:
		converted.Spec, err = eventRuleSendAlertFrom(spec.SendAlert)
	case *pb.EventRuleAction_Noop:
		if spec.Noop == nil {
			err = fmt.Errorf("noop action is required")
		} else {
			converted.Spec = &eventrule.Noop{Reason: spec.Noop.GetReason()}
		}
	default:
		err = fmt.Errorf("action spec is required")
	}
	if err != nil {
		return eventrule.Action{}, err
	}
	if err := converted.Validate(); err != nil {
		return eventrule.Action{}, err
	}

	return converted, nil
}

func eventRuleActionConditionFrom(
	condition *pb.EventRuleActionCondition,
) (eventrule.ActionCondition, error) {
	if condition == nil {
		return eventrule.ActionCondition{}, nil
	}

	converted := eventrule.ActionCondition{}
	if condition.Severities != nil {
		converted.Severities = make([]eventrule.Severity, len(condition.Severities))
		for i, severity := range condition.Severities {
			value, err := eventRuleSeverityFrom(severity, false)
			if err != nil {
				return eventrule.ActionCondition{}, fmt.Errorf("condition severities[%d]: %w", i, err)
			}
			converted.Severities[i] = value
		}
	}
	if condition.ComponentTypes != nil {
		converted.ComponentTypes = make([]flowtypes.ComponentType, len(condition.ComponentTypes))
		for i, componentType := range condition.ComponentTypes {
			value, err := eventRuleComponentTypeFrom(componentType)
			if err != nil {
				return eventrule.ActionCondition{}, fmt.Errorf(
					"condition component_types[%d]: %w",
					i,
					err,
				)
			}
			converted.ComponentTypes[i] = value
		}
	}

	return converted, nil
}

func eventRuleSubmitTaskFrom(
	spec *pb.EventRuleSubmitTaskAction,
) (*eventrule.SubmitTask, error) {
	if spec == nil {
		return nil, fmt.Errorf("submit_task action is required")
	}

	operation, err := TaskOperationFrom(spec.GetOperation())
	if err != nil {
		return nil, fmt.Errorf("operation: %w", err)
	}

	targetStrategy, err := eventRuleTargetStrategyFrom(spec.GetTargetStrategy())
	if err != nil {
		return nil, err
	}
	conflictStrategy, err := eventRuleConflictStrategyFrom(spec.GetConflictStrategy())
	if err != nil {
		return nil, err
	}

	return &eventrule.SubmitTask{
		Operation:        operation,
		TargetStrategy:   targetStrategy,
		ConflictStrategy: conflictStrategy,
		Description:      spec.GetDescription(),
	}, nil
}

func eventRuleSendAlertFrom(
	spec *pb.EventRuleSendAlertAction,
) (*eventrule.SendAlert, error) {
	if spec == nil {
		return nil, fmt.Errorf("send_alert action is required")
	}

	severity, err := eventRuleSeverityFrom(spec.GetSeverity(), false)
	if err != nil {
		return nil, err
	}

	return &eventrule.SendAlert{Severity: severity, Message: spec.GetMessage()}, nil
}

// EventRuleTo converts one domain rule to its API representation.
func EventRuleTo(rule *eventrule.Rule) (*pb.EventRule, error) {
	if rule == nil {
		return nil, fmt.Errorf("event rule is required")
	}
	if err := rule.Validate(); err != nil {
		return nil, err
	}

	actions := make([]*pb.EventRuleAction, len(rule.Actions))
	for i := range rule.Actions {
		action, err := eventRuleActionTo(rule.Actions[i])
		if err != nil {
			return nil, fmt.Errorf("actions[%d]: %w", i, err)
		}
		actions[i] = action
	}

	return &pb.EventRule{
		Id:          UUIDTo(rule.ID),
		ReadOnly:    rule.Origin == eventrule.RuleOriginBuiltIn,
		Name:        rule.Name,
		Description: rule.Description,
		Enabled:     rule.Enabled,
		EventType:   string(rule.EventType),
		Actions:     actions,
		CreatedAt:   eventRuleTimestampTo(rule.CreatedAt),
		UpdatedAt:   eventRuleTimestampTo(rule.UpdatedAt),
	}, nil
}

func eventRuleActionTo(action eventrule.Action) (*pb.EventRuleAction, error) {
	converted := &pb.EventRuleAction{
		Name:      action.Name,
		Condition: eventRuleActionConditionTo(action.Condition),
	}

	switch spec := action.Spec.(type) {
	case *eventrule.SubmitTask:
		if spec == nil || spec.Operation == nil {
			return nil, fmt.Errorf("submit_task action is required")
		}
		operation, err := TaskOperationTo(spec.Operation)
		if err != nil {
			return nil, err
		}
		converted.Spec = &pb.EventRuleAction_SubmitTask{
			SubmitTask: &pb.EventRuleSubmitTaskAction{
				Operation:        operation,
				TargetStrategy:   eventRuleTargetStrategyTo(spec.TargetStrategy),
				ConflictStrategy: eventRuleConflictStrategyTo(spec.ConflictStrategy),
				Description:      spec.Description,
			},
		}
	case *eventrule.SendAlert:
		if spec == nil {
			return nil, fmt.Errorf("send_alert action is required")
		}
		converted.Spec = &pb.EventRuleAction_SendAlert{
			SendAlert: &pb.EventRuleSendAlertAction{
				Severity: eventRuleSeverityTo(spec.Severity),
				Message:  spec.Message,
			},
		}
	case *eventrule.Noop:
		if spec == nil {
			return nil, fmt.Errorf("noop action is required")
		}
		converted.Spec = &pb.EventRuleAction_Noop{
			Noop: &pb.EventRuleNoopAction{Reason: spec.Reason},
		}
	default:
		return nil, fmt.Errorf("unsupported action spec %T", action.Spec)
	}

	return converted, nil
}

func eventRuleActionConditionTo(
	condition eventrule.ActionCondition,
) *pb.EventRuleActionCondition {
	converted := &pb.EventRuleActionCondition{}
	if condition.Severities != nil {
		converted.Severities = make([]pb.EventRuleSeverity, len(condition.Severities))
		for i, severity := range condition.Severities {
			converted.Severities[i] = eventRuleSeverityTo(severity)
		}
	}
	if condition.ComponentTypes != nil {
		converted.ComponentTypes = make([]pb.ComponentType, len(condition.ComponentTypes))
		for i, componentType := range condition.ComponentTypes {
			converted.ComponentTypes[i] = eventRuleComponentTypeTo(componentType)
		}
	}

	return converted
}

// EventRuleScopeFrom converts and validates a site or rack scope.
func EventRuleScopeFrom(scope *pb.EventRuleScope) (eventrule.Scope, error) {
	if scope == nil {
		return eventrule.Scope{}, fmt.Errorf("event rule scope is required")
	}

	converted := eventrule.Scope{}
	switch scope.GetType() {
	case pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE:
		converted.Type = eventrule.ScopeTypeSite
		if scope.GetId() != nil {
			return eventrule.Scope{}, fmt.Errorf("site scope must not have an id")
		}
	case pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK:
		converted.Type = eventrule.ScopeTypeRack
		if scope.GetId() == nil || scope.GetId().GetId() == "" {
			return eventrule.Scope{}, fmt.Errorf("rack scope requires an id")
		}
		id, err := uuid.Parse(scope.GetId().GetId())
		if err != nil || id == uuid.Nil {
			return eventrule.Scope{}, fmt.Errorf("rack scope id must be a valid non-zero UUID")
		}
		converted.ID = id
	default:
		return eventrule.Scope{}, fmt.Errorf("unknown event rule scope type %q", scope.GetType())
	}
	if err := converted.Validate(); err != nil {
		return eventrule.Scope{}, err
	}

	return converted, nil
}

// EventRuleScopeTo converts one validated domain scope.
func EventRuleScopeTo(scope eventrule.Scope) (*pb.EventRuleScope, error) {
	if err := scope.Validate(); err != nil {
		return nil, err
	}

	converted := &pb.EventRuleScope{}
	switch scope.Type {
	case eventrule.ScopeTypeSite:
		converted.Type = pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE
	case eventrule.ScopeTypeRack:
		converted.Type = pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK
		converted.Id = UUIDTo(scope.ID)
	default:
		return nil, fmt.Errorf("unknown event rule scope type %q", scope.Type)
	}

	return converted, nil
}

// EventRuleBindingTo converts one binding to its API representation.
func EventRuleBindingTo(binding *eventrule.Binding) (*pb.EventRuleBinding, error) {
	if binding == nil {
		return nil, fmt.Errorf("event rule binding is required")
	}
	if err := binding.Validate(); err != nil {
		return nil, err
	}

	scope, err := EventRuleScopeTo(binding.Scope)
	if err != nil {
		return nil, err
	}

	return &pb.EventRuleBinding{
		Id:        UUIDTo(binding.ID),
		RuleId:    UUIDTo(binding.RuleID),
		EventType: string(binding.EventType),
		Scope:     scope,
	}, nil
}

func eventRuleSeverityFrom(
	severity pb.EventRuleSeverity,
	allowUnspecified bool,
) (eventrule.Severity, error) {
	switch severity {
	case pb.EventRuleSeverity_EVENT_RULE_SEVERITY_UNSPECIFIED:
		if allowUnspecified {
			return eventrule.SeverityUnspecified, nil
		}
	case pb.EventRuleSeverity_EVENT_RULE_SEVERITY_INFO:
		return eventrule.SeverityInfo, nil
	case pb.EventRuleSeverity_EVENT_RULE_SEVERITY_WARNING:
		return eventrule.SeverityWarning, nil
	case pb.EventRuleSeverity_EVENT_RULE_SEVERITY_CRITICAL:
		return eventrule.SeverityCritical, nil
	}

	return eventrule.SeverityUnspecified, fmt.Errorf("unknown event rule severity %q", severity)
}

func eventRuleSeverityTo(severity eventrule.Severity) pb.EventRuleSeverity {
	switch severity {
	case eventrule.SeverityInfo:
		return pb.EventRuleSeverity_EVENT_RULE_SEVERITY_INFO
	case eventrule.SeverityWarning:
		return pb.EventRuleSeverity_EVENT_RULE_SEVERITY_WARNING
	case eventrule.SeverityCritical:
		return pb.EventRuleSeverity_EVENT_RULE_SEVERITY_CRITICAL
	default:
		return pb.EventRuleSeverity_EVENT_RULE_SEVERITY_UNSPECIFIED
	}
}

func eventRuleTargetStrategyFrom(
	strategy pb.EventRuleTargetStrategy,
) (eventrule.TargetStrategy, error) {
	switch strategy {
	case pb.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_COMPONENT:
		return eventrule.TargetStrategyComponent, nil
	case pb.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_RACK:
		return eventrule.TargetStrategyRack, nil
	case pb.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_AFFECTED_COMPONENTS:
		return eventrule.TargetStrategyAffectedComponents, nil
	default:
		return "", fmt.Errorf("unknown event rule target strategy %q", strategy)
	}
}

func eventRuleTargetStrategyTo(strategy eventrule.TargetStrategy) pb.EventRuleTargetStrategy {
	switch strategy {
	case eventrule.TargetStrategyComponent:
		return pb.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_COMPONENT
	case eventrule.TargetStrategyRack:
		return pb.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_RACK
	case eventrule.TargetStrategyAffectedComponents:
		return pb.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_AFFECTED_COMPONENTS
	default:
		return pb.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_UNSPECIFIED
	}
}

func eventRuleConflictStrategyFrom(
	strategy pb.EventRuleConflictStrategy,
) (eventrule.ConflictStrategy, error) {
	switch strategy {
	case pb.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_QUEUE:
		return eventrule.ConflictStrategyQueue, nil
	case pb.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_REJECT:
		return eventrule.ConflictStrategyReject, nil
	default:
		return "", fmt.Errorf("unknown event rule conflict strategy %q", strategy)
	}
}

func eventRuleConflictStrategyTo(
	strategy eventrule.ConflictStrategy,
) pb.EventRuleConflictStrategy {
	switch strategy {
	case eventrule.ConflictStrategyQueue:
		return pb.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_QUEUE
	case eventrule.ConflictStrategyReject:
		return pb.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_REJECT
	default:
		return pb.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_UNSPECIFIED
	}
}

func eventRuleComponentTypeFrom(componentType pb.ComponentType) (flowtypes.ComponentType, error) {
	var converted flowtypes.ComponentType
	switch componentType {
	case pb.ComponentType_COMPONENT_TYPE_COMPUTE:
		converted = flowtypes.ComponentTypeCompute
	case pb.ComponentType_COMPONENT_TYPE_NVSWITCH:
		converted = flowtypes.ComponentTypeNVSwitch
	case pb.ComponentType_COMPONENT_TYPE_POWERSHELF:
		converted = flowtypes.ComponentTypePowerShelf
	case pb.ComponentType_COMPONENT_TYPE_TORSWITCH:
		converted = flowtypes.ComponentTypeTORSwitch
	case pb.ComponentType_COMPONENT_TYPE_UMS:
		converted = flowtypes.ComponentTypeUMS
	case pb.ComponentType_COMPONENT_TYPE_CDU:
		converted = flowtypes.ComponentTypeCDU
	default:
		return flowtypes.ComponentTypeUnknown, fmt.Errorf("unknown component type %q", componentType)
	}

	return converted, nil
}

func eventRuleComponentTypeTo(componentType flowtypes.ComponentType) pb.ComponentType {
	switch componentType {
	case flowtypes.ComponentTypeCompute:
		return pb.ComponentType_COMPONENT_TYPE_COMPUTE
	case flowtypes.ComponentTypeNVSwitch:
		return pb.ComponentType_COMPONENT_TYPE_NVSWITCH
	case flowtypes.ComponentTypePowerShelf:
		return pb.ComponentType_COMPONENT_TYPE_POWERSHELF
	case flowtypes.ComponentTypeTORSwitch:
		return pb.ComponentType_COMPONENT_TYPE_TORSWITCH
	case flowtypes.ComponentTypeUMS:
		return pb.ComponentType_COMPONENT_TYPE_UMS
	case flowtypes.ComponentTypeCDU:
		return pb.ComponentType_COMPONENT_TYPE_CDU
	default:
		return pb.ComponentType_COMPONENT_TYPE_UNKNOWN
	}
}

func eventRuleTimestampTo(value time.Time) *timestamppb.Timestamp {
	if value.IsZero() {
		return nil
	}

	return timestamppb.New(value)
}
