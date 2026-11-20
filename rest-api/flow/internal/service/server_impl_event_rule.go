// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"errors"
	"fmt"

	"github.com/google/uuid"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/emptypb"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/protobuf"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

type eventRuleManager interface {
	PutBinding(context.Context, eventrule.Type, uuid.UUID, eventrule.Scope) (*eventrule.BindingPutResult, error)
	ListBindings(context.Context, eventrule.BindingListRequest) ([]eventrule.Binding, int, error)
	ValidateEventType(eventrule.Type) error
	ListEventHandlings(context.Context, eventrule.HandlingQuery) ([]eventrule.Handling, int, error)
	Create(context.Context, eventrule.RuleCreate) (*eventrule.Rule, error)
	GetByID(context.Context, uuid.UUID) (*eventrule.Rule, error)
	GetEffective(context.Context, eventrule.Type, eventrule.ResourceIdentity) (*eventrule.Rule, error)
	List(context.Context, eventrule.RuleListRequest) (eventrule.RuleListPage, error)
	SetEnabled(context.Context, uuid.UUID, bool) error
	Delete(context.Context, uuid.UUID) error
	Bind(context.Context, uuid.UUID, eventrule.Scope) (*eventrule.Binding, error)
	GetBindingForScope(
		context.Context,
		eventrule.Type,
		eventrule.Scope,
	) (*eventrule.Binding, error)
	Unbind(context.Context, eventrule.Type, eventrule.Scope) error
}

func (rs *FlowServerImpl) requireEventRuleManager() (eventRuleManager, error) {
	if rs == nil || rs.eventRuleManager == nil {
		return nil, status.Error(codes.FailedPrecondition, "event rule manager is not configured")
	}

	return rs.eventRuleManager, nil
}

func (rs *FlowServerImpl) CreateEventRule(
	ctx context.Context,
	req *pb.CreateEventRuleRequest,
) (*pb.EventRule, error) {
	manager, err := rs.requireEventRuleManager()
	if err != nil {
		return nil, err
	}

	input, err := protobuf.EventRuleCreateFrom(req)
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}

	rule, err := manager.Create(ctx, input)
	if err != nil {
		return nil, eventRuleStatusError(codes.Internal, err)
	}

	return eventRuleResponse(rule)
}

func (rs *FlowServerImpl) GetEventRule(
	ctx context.Context,
	req *pb.GetEventRuleRequest,
) (*pb.EventRule, error) {
	manager, err := rs.requireEventRuleManager()
	if err != nil {
		return nil, err
	}

	id, err := requiredEventRuleUUID(req.GetRuleId(), "event rule ID")
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}

	rule, err := manager.GetByID(ctx, id)
	if err != nil {
		return nil, eventRuleStatusError(codes.Internal, err)
	}

	return eventRuleResponse(rule)
}

func (rs *FlowServerImpl) GetEffectiveEventRule(
	ctx context.Context,
	req *pb.GetEffectiveEventRuleRequest,
) (*pb.EventRule, error) {
	manager, err := rs.requireEventRuleManager()
	if err != nil {
		return nil, err
	}

	eventType := eventrule.Type(req.GetEventType())
	if err := eventType.Validate(); err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}

	target, err := eventRuleTargetFrom(req)
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}

	rule, err := manager.GetEffective(ctx, eventType, target)
	if err != nil {
		return nil, eventRuleStatusError(codes.Internal, err)
	}

	if rule == nil {
		return nil, status.Error(codes.NotFound, "effective event rule was not found")
	}

	return eventRuleResponse(rule)
}

func (rs *FlowServerImpl) ListEventRules(
	ctx context.Context,
	req *pb.ListEventRulesRequest,
) (*pb.ListEventRulesResponse, error) {
	manager, err := rs.requireEventRuleManager()
	if err != nil {
		return nil, err
	}

	request, err := protobuf.EventRuleListRequestFrom(req)
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}

	page, err := manager.List(ctx, request)
	if err != nil {
		return nil, eventRuleStatusError(codes.Internal, err)
	}

	converted := make([]*pb.EventRule, len(page.Rules))
	for i, rule := range page.Rules {
		converted[i], err = protobuf.EventRuleTo(rule)
		if err != nil {
			return nil, status.Error(codes.Internal, err.Error())
		}
	}

	return &pb.ListEventRulesResponse{
		Rules: converted,
		Total: int64(page.Total),
	}, nil
}

// SetEventRuleEnabled applies an explicit enabled state to an immutable policy.
func (rs *FlowServerImpl) SetEventRuleEnabled(
	ctx context.Context,
	req *pb.SetEventRuleEnabledRequest,
) (*pb.EventRule, error) {
	manager, err := rs.requireEventRuleManager()
	if err != nil {
		return nil, err
	}
	id, err := requiredEventRuleUUID(req.GetRuleId(), "event rule ID")
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}
	if req.Enabled == nil {
		return nil, status.Error(codes.InvalidArgument, "enabled is required")
	}
	err = manager.SetEnabled(ctx, id, req.GetEnabled())
	if err != nil {
		return nil, eventRuleStatusError(codes.Internal, err)
	}
	rule, err := manager.GetByID(ctx, id)
	if err != nil {
		return nil, eventRuleStatusError(codes.Internal, err)
	}
	return eventRuleResponse(rule)
}

func (rs *FlowServerImpl) DeleteEventRule(
	ctx context.Context,
	req *pb.DeleteEventRuleRequest,
) (*emptypb.Empty, error) {
	manager, err := rs.requireEventRuleManager()
	if err != nil {
		return nil, err
	}

	id, err := requiredEventRuleUUID(req.GetRuleId(), "event rule ID")
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}
	err = manager.Delete(ctx, id)
	if errors.Is(err, eventrule.ErrBindingConflict) {
		return nil, status.Error(codes.AlreadyExists, "policy still has bindings")
	}
	if err != nil {
		return nil, eventRuleStatusError(codes.Internal, err)
	}

	return &emptypb.Empty{}, nil
}

func (rs *FlowServerImpl) CreateEventRuleBinding(
	ctx context.Context,
	req *pb.CreateEventRuleBindingRequest,
) (*pb.EventRuleBinding, error) {
	manager, err := rs.requireEventRuleManager()
	if err != nil {
		return nil, err
	}

	ruleID, err := requiredEventRuleUUID(req.GetRuleId(), "event rule ID")
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}

	scope, err := protobuf.EventRuleScopeFrom(req.GetScope())
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}

	binding, err := manager.Bind(ctx, ruleID, scope)
	if err != nil {
		return nil, eventRuleStatusError(codes.Internal, err)
	}

	return eventRuleBindingResponse(binding)
}

func (rs *FlowServerImpl) GetEventRuleBinding(
	ctx context.Context,
	req *pb.GetEventRuleBindingRequest,
) (*pb.EventRuleBinding, error) {
	manager, err := rs.requireEventRuleManager()
	if err != nil {
		return nil, err
	}

	eventType := eventrule.Type(req.GetEventType())
	if err := eventType.Validate(); err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}
	scope, err := rs.bindingScope(ctx, req.GetScope(), req.GetSelector())
	if err != nil {
		return nil, eventRuleStatusError(codes.InvalidArgument, err)
	}

	binding, err := manager.GetBindingForScope(ctx, eventType, scope)
	if err != nil {
		return nil, eventRuleStatusError(codes.Internal, err)
	}
	if binding == nil {
		return nil, status.Error(codes.NotFound, "event rule binding was not found")
	}

	result, err := eventRuleBindingResponse(binding)
	if err == nil {
		rs.bindingRackID(ctx, result)
	}
	return result, err
}

func (rs *FlowServerImpl) DeleteEventRuleBinding(
	ctx context.Context,
	req *pb.DeleteEventRuleBindingRequest,
) (*emptypb.Empty, error) {
	manager, err := rs.requireEventRuleManager()
	if err != nil {
		return nil, err
	}

	eventType := eventrule.Type(req.GetEventType())
	if err := eventType.Validate(); err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}
	scope, err := rs.bindingScope(ctx, req.GetScope(), req.GetSelector())
	if err != nil {
		return nil, eventRuleStatusError(codes.InvalidArgument, err)
	}

	if err := manager.Unbind(ctx, eventType, scope); err != nil {
		return nil, eventRuleStatusError(codes.Internal, err)
	}

	return &emptypb.Empty{}, nil
}

func (rs *FlowServerImpl) resolveEventRuleScope(
	ctx context.Context,
	selector *pb.EventRuleScopeSelector,
) (eventrule.Scope, error) {
	if selector == nil {
		return eventrule.Scope{}, status.Error(codes.InvalidArgument, "event rule scope is required")
	}

	canonical := &pb.EventRuleScope{Type: selector.GetType()}
	switch selector.GetType() {
	case pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE:
		if selector.GetRackIdentifier() != nil {
			return eventrule.Scope{}, status.Error(
				codes.InvalidArgument,
				"site scope must not include a rack identifier",
			)
		}
	case pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK:
		switch identifier := selector.GetRackIdentifier().(type) {
		case *pb.EventRuleScopeSelector_RackId:
			canonical.Id = identifier.RackId
		case *pb.EventRuleScopeSelector_RackExternalId:
			if identifier.RackExternalId == "" {
				return eventrule.Scope{}, status.Error(
					codes.InvalidArgument,
					"rack external ID is required",
				)
			}
			if rs.inventoryManager == nil {
				return eventrule.Scope{}, status.Error(
					codes.FailedPrecondition,
					"inventory manager is not configured",
				)
			}
			rack, err := rs.inventoryManager.GetRackByIdentifier(
				ctx,
				externalRackIdentifier(identifier.RackExternalId),
				false,
			)
			if err != nil {
				return eventrule.Scope{}, err
			}
			canonical.Id = protobuf.UUIDTo(rack.Info.ID)
		default:
			return eventrule.Scope{}, status.Error(
				codes.InvalidArgument,
				"rack scope requires a rack ID or rack external ID",
			)
		}
	default:
		return eventrule.Scope{}, status.Errorf(
			codes.InvalidArgument,
			"unknown event rule scope type %q",
			selector.GetType(),
		)
	}

	scope, err := protobuf.EventRuleScopeFrom(canonical)
	if err != nil {
		return eventrule.Scope{}, status.Error(codes.InvalidArgument, err.Error())
	}

	return scope, nil
}

func (rs *FlowServerImpl) resolveEventHandlingResource(
	ctx context.Context,
	resourceType pb.EventResourceType,
	resourceID string,
) (eventrule.ResourceKind, uuid.UUID, error) {
	switch resourceType {
	case pb.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT:
		component, err := rs.resolveComponentIdentifier(ctx, resourceID)
		if err != nil {
			return "", uuid.Nil, err
		}
		return eventrule.ResourceKindComponent, component.Info.ID, nil
	case pb.EventResourceType_EVENT_RESOURCE_TYPE_RACK:
		rack, err := rs.inventoryManager.GetRackByIdentifier(
			ctx,
			externalRackIdentifier(resourceID),
			false,
		)
		if err != nil {
			return "", uuid.Nil, err
		}
		return eventrule.ResourceKindRack, rack.Info.ID, nil
	default:
		return "", uuid.Nil, status.Error(
			codes.InvalidArgument,
			"resource_type must be COMPONENT or RACK",
		)
	}
}

func requiredEventRuleUUID(value *pb.UUID, name string) (uuid.UUID, error) {
	if value == nil || value.GetId() == "" {
		return uuid.Nil, fmt.Errorf("%s is required", name)
	}

	id, err := uuid.Parse(value.GetId())
	if err != nil || id == uuid.Nil {
		return uuid.Nil, fmt.Errorf("%s must be a valid non-zero UUID", name)
	}

	return id, nil
}

func eventRuleTargetFrom(
	req *pb.GetEffectiveEventRuleRequest,
) (eventrule.ResourceIdentity, error) {
	if req == nil {
		return eventrule.ResourceIdentity{}, fmt.Errorf("event rule target is required")
	}

	var kind eventrule.ResourceKind
	var value *pb.UUID
	var name string
	switch target := req.GetTarget().(type) {
	case *pb.GetEffectiveEventRuleRequest_RackId:
		kind = eventrule.ResourceKindRack
		value = target.RackId
		name = "rack ID"
	case *pb.GetEffectiveEventRuleRequest_ComponentId:
		kind = eventrule.ResourceKindComponent
		value = target.ComponentId
		name = "component ID"
	default:
		return eventrule.ResourceIdentity{}, fmt.Errorf("event rule target is required")
	}

	id, err := requiredEventRuleUUID(value, name)
	if err != nil {
		return eventrule.ResourceIdentity{}, err
	}

	return eventrule.ResourceIdentity{Kind: kind, ID: id}, nil
}

func eventRuleResponse(rule *eventrule.Rule) (*pb.EventRule, error) {
	converted, err := protobuf.EventRuleTo(rule)
	if err != nil {
		return nil, status.Error(codes.Internal, err.Error())
	}

	return converted, nil
}

func eventRuleBindingResponse(binding *eventrule.Binding) (*pb.EventRuleBinding, error) {
	converted, err := protobuf.EventRuleBindingTo(binding)
	if err != nil {
		return nil, status.Error(codes.Internal, err.Error())
	}

	return converted, nil
}

func eventRuleStatusError(defaultCode codes.Code, err error) error {
	if _, ok := status.FromError(err); ok {
		return err
	}

	code := defaultCode
	switch {
	case errors.Is(err, eventrule.ErrInvalidRuleInput):
		code = codes.InvalidArgument
	case errors.Is(err, eventrule.ErrRuleTargetNotFound):
		code = codes.NotFound
	case errors.Is(err, eventrule.ErrRuleNotFound),
		errors.Is(err, eventrule.ErrBindingNotFound):
		code = codes.NotFound
	case errors.Is(err, eventrule.ErrBuiltInRuleImmutable),
		errors.Is(err, eventrule.ErrBindingConflict):
		code = codes.FailedPrecondition
	}

	return status.Error(code, err.Error())
}

func (rs *FlowServerImpl) bindingScope(ctx context.Context, scope *pb.EventRuleScope, selector *pb.EventRuleScopeSelector) (eventrule.Scope, error) {
	if scope != nil && selector != nil {
		return eventrule.Scope{}, status.Error(codes.InvalidArgument, "scope and selector are mutually exclusive")
	}
	if selector != nil {
		return rs.resolveEventRuleScope(ctx, selector)
	}
	return protobuf.EventRuleScopeFrom(scope)
}

// PutEventRuleBinding atomically selects an existing policy for a scope.
func (rs *FlowServerImpl) PutEventRuleBinding(ctx context.Context, req *pb.PutEventRuleBindingRequest) (*pb.PutEventRuleBindingResponse, error) {
	manager, err := rs.requireEventRuleManager()
	if err != nil {
		return nil, err
	}
	id, err := requiredEventRuleUUID(req.GetRuleId(), "policy ID")
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}
	scope, err := rs.resolveEventRuleScope(ctx, req.GetScope())
	if err != nil {
		return nil, err
	}
	binding, err := manager.PutBinding(ctx, eventrule.Type(req.GetEventType()), id, scope)
	if errors.Is(err, eventrule.ErrRuleNotFound) {
		return nil, status.Error(codes.InvalidArgument, "policy specified in body does not exist")
	}
	if err != nil {
		return nil, eventRuleStatusError(codes.Internal, err)
	}
	result, err := eventRuleBindingResponse(binding.Binding)
	if err == nil {
		rs.bindingRackID(ctx, result)
	}
	if err != nil {
		return nil, err
	}
	return &pb.PutEventRuleBindingResponse{Binding: result, Created: binding.Created}, nil
}

func (rs *FlowServerImpl) bindingRackID(ctx context.Context, binding *pb.EventRuleBinding) {
	if binding.GetScope().GetType() != pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK || rs.inventoryManager == nil {
		return
	}
	id, err := uuid.Parse(binding.GetScope().GetId().GetId())
	if err != nil {
		return
	}
	rack, err := rs.inventoryManager.GetRackByIdentifier(ctx, identifier.Identifier{ID: id}, false)
	// Keep the canonical scope UUID if inventory has disappeared.
	if err == nil && rack != nil && rack.ExternalID != "" {
		binding.RackExternalId = &rack.ExternalID
	}
}

// ListEventRuleBindings lists explicit references without resolving fallback policies.
func (rs *FlowServerImpl) ListEventRuleBindings(ctx context.Context, req *pb.ListEventRuleBindingsRequest) (*pb.ListEventRuleBindingsResponse, error) {
	manager, err := rs.requireEventRuleManager()
	if err != nil {
		return nil, err
	}
	page := protobuf.PaginationFrom(req.GetPagination())
	err = page.Validate()
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}
	query := eventrule.BindingListRequest{Offset: page.Offset, Limit: page.Limit}
	if req.GetRuleId() != nil {
		query.RuleID, err = requiredEventRuleUUID(req.GetRuleId(), "policy ID")
		if err != nil {
			return nil, status.Error(codes.InvalidArgument, err.Error())
		}
	}
	if req.EventType != nil {
		if req.GetEventType() == "" {
			return nil, status.Error(codes.InvalidArgument, "event_type must not be empty")
		}
		query.EventType = eventrule.Type(req.GetEventType())
		err = manager.ValidateEventType(query.EventType)
		if err != nil {
			return nil, eventRuleStatusError(codes.InvalidArgument, err)
		}
	}
	switch req.GetScopeType() {
	case pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_UNSPECIFIED:
	case pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE:
		query.ScopeType = eventrule.ScopeTypeSite
	case pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK:
		query.ScopeType = eventrule.ScopeTypeRack
	default:
		return nil, status.Error(codes.InvalidArgument, "unknown binding scope type")
	}
	if req.RackId != nil {
		if req.GetRackId() == "" || query.ScopeType == eventrule.ScopeTypeSite {
			return nil, status.Error(codes.InvalidArgument, "rack_id requires a nonempty Rack selector")
		}
		selector := &pb.EventRuleScopeSelector{
			Type:           pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK,
			RackIdentifier: &pb.EventRuleScopeSelector_RackExternalId{RackExternalId: req.GetRackId()},
		}
		scope, resolveErr := rs.resolveEventRuleScope(ctx, selector)
		if status.Code(resolveErr) == codes.NotFound {
			return &pb.ListEventRuleBindingsResponse{Bindings: []*pb.EventRuleBinding{}}, nil
		}
		if resolveErr != nil {
			return nil, resolveErr
		}
		query.RackID = scope.ID
	}
	err = query.Validate()
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}
	rows, total, err := manager.ListBindings(ctx, query)
	if err != nil {
		return nil, eventRuleStatusError(codes.Internal, err)
	}
	result := &pb.ListEventRuleBindingsResponse{Total: int64(total), Bindings: make([]*pb.EventRuleBinding, 0, len(rows))}
	for i := range rows {
		binding, err := eventRuleBindingResponse(&rows[i])
		if err != nil {
			return nil, err
		}
		rs.bindingRackID(ctx, binding)
		result.Bindings = append(result.Bindings, binding)
	}
	return result, nil
}

const maxEventHandlingLimit = 100

func (rs *FlowServerImpl) eventHandlingQuery(ctx context.Context, filter *pb.EventHandlingFilter, page *pb.Pagination) (eventrule.HandlingQuery, error) {
	pagination := protobuf.PaginationFrom(page)
	err := pagination.Validate()
	if err != nil || pagination.Limit > maxEventHandlingLimit {
		return eventrule.HandlingQuery{}, status.Error(codes.InvalidArgument, "invalid pagination")
	}
	query := eventrule.HandlingQuery{Offset: pagination.Offset, Limit: pagination.Limit}
	if filter.GetEventType() != "" {
		err = rs.eventRuleManager.ValidateEventType(eventrule.Type(filter.GetEventType()))
		if err != nil {
			return query, eventRuleStatusError(codes.InvalidArgument, err)
		}
		query.EventType = eventrule.Type(filter.GetEventType())
	} else if filter != nil && filter.EventType != nil {
		return query, status.Error(codes.InvalidArgument, "event_type must not be empty")
	}
	if filter.GetPolicyId() != nil {
		query.PolicyID, err = requiredEventRuleUUID(filter.GetPolicyId(), "policy ID")
		if err != nil {
			return query, status.Error(codes.InvalidArgument, err.Error())
		}
	}
	if filter.GetRackId() != "" {
		selector := &pb.EventRuleScopeSelector{
			Type:           pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK,
			RackIdentifier: &pb.EventRuleScopeSelector_RackExternalId{RackExternalId: filter.GetRackId()},
		}
		scope, resolveErr := rs.resolveEventRuleScope(ctx, selector)
		if resolveErr != nil {
			return query, resolveErr
		}
		query.RackID = scope.ID
	} else if filter != nil && filter.RackId != nil {
		return query, status.Error(codes.InvalidArgument, "rack_id must not be empty")
	}
	if filter != nil && filter.ResourceId != nil && filter.GetResourceId() == "" {
		return query, status.Error(codes.InvalidArgument, "resource_id must not be empty")
	}
	if filter.GetResourceId() != "" || filter.GetResourceType() != pb.EventResourceType_EVENT_RESOURCE_TYPE_UNSPECIFIED {
		if filter.GetResourceId() == "" {
			return query, status.Error(codes.InvalidArgument, "source ID is required with source type")
		}
		query.ResourceType, query.ResourceID, err = rs.resolveEventHandlingResource(ctx, filter.GetResourceType(), filter.GetResourceId())
		if err != nil {
			return query, err
		}
	}
	return query, nil
}

// ListEventHandlings returns aggregate results across matching captured events.
func (rs *FlowServerImpl) ListEventHandlings(ctx context.Context, req *pb.ListEventHandlingsRequest) (*pb.ListEventHandlingsResponse, error) {
	manager, err := rs.requireEventRuleManager()
	if err != nil {
		return nil, err
	}
	query, err := rs.eventHandlingQuery(ctx, req.GetFilter(), req.GetPagination())
	if err != nil {
		return nil, err
	}
	rows, total, err := manager.ListEventHandlings(ctx, query)
	if err != nil {
		return nil, eventRuleStatusError(codes.Internal, err)
	}
	result := &pb.ListEventHandlingsResponse{TotalCount: int32(total), Handlings: make([]*pb.EventHandling, 0, len(rows))}
	for _, row := range rows {
		value, err := protobuf.EventHandlingTo(row)
		if err != nil {
			return nil, status.Error(codes.Internal, "Failed to render event handling")
		}
		result.Handlings = append(result.Handlings, value)
	}
	return result, nil
}

// ListEventTasks reports Tasks linked through their action execution provenance.
func (rs *FlowServerImpl) ListEventTasks(ctx context.Context, req *pb.ListEventTasksRequest) (*pb.ListTasksResponse, error) {
	_, err := rs.requireEventRuleManager()
	if err != nil {
		return nil, err
	}
	query, err := rs.eventHandlingQuery(ctx, req.GetFilter(), req.GetPagination())
	if err != nil {
		return nil, err
	}
	if rs.taskStore == nil {
		return nil, status.Error(codes.FailedPrecondition, "Task store is not configured")
	}
	rows, total, err := rs.taskStore.ListEventTasks(ctx, query, req.GetActiveOnly())
	if err != nil {
		return nil, err
	}
	result := &pb.ListTasksResponse{Total: total, Tasks: make([]*pb.Task, 0, len(rows))}
	for _, row := range rows {
		result.Tasks = append(result.Tasks, protobuf.TaskTo(row))
	}
	stripTaskReports(result.Tasks)
	return result, nil
}
