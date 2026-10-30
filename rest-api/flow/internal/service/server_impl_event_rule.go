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
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

type eventRuleManager interface {
	Create(context.Context, eventrule.RuleCreate) (*eventrule.Rule, error)
	GetByID(context.Context, uuid.UUID) (*eventrule.Rule, error)
	GetEffective(context.Context, eventrule.Type, eventrule.ResourceIdentity) (*eventrule.Rule, error)
	List(context.Context, eventrule.RuleListRequest) (eventrule.RuleListPage, error)
	UpdateMetadata(context.Context, uuid.UUID, eventrule.RuleMetadata) error
	ReplaceActions(context.Context, uuid.UUID, []eventrule.Action) error
	SetEnabled(context.Context, uuid.UUID, bool) error
	Delete(context.Context, uuid.UUID) error
	Bind(context.Context, uuid.UUID, eventrule.Scope) (*eventrule.Binding, error)
	GetBindingForScope(
		context.Context,
		eventrule.Type,
		eventrule.Scope,
	) (*eventrule.Binding, error)
	Unbind(context.Context, eventrule.Type, eventrule.Scope) error
	SetOverride(context.Context, eventrule.RuleCreate, eventrule.Scope) (*eventrule.Rule, error)
	GetEffectiveForScope(context.Context, eventrule.Type, eventrule.Scope) (*eventrule.Rule, error)
	DeleteOverride(context.Context, eventrule.Type, eventrule.Scope) error
	GetEventHandling(context.Context, eventrule.Type, eventrule.ResourceKind, uuid.UUID, int, *eventrule.HandlingCursor) ([]eventrule.Handling, bool, error)
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

func (rs *FlowServerImpl) UpdateEventRule(
	ctx context.Context,
	req *pb.UpdateEventRuleRequest,
) (*pb.EventRule, error) {
	manager, err := rs.requireEventRuleManager()
	if err != nil {
		return nil, err
	}

	id, err := requiredEventRuleUUID(req.GetRuleId(), "event rule ID")
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}

	switch update := req.GetUpdate().(type) {
	case *pb.UpdateEventRuleRequest_Metadata:
		if update.Metadata == nil {
			return nil, status.Error(
				codes.InvalidArgument,
				"event rule metadata update is required",
			)
		}
		err = manager.UpdateMetadata(ctx, id, eventrule.RuleMetadata{
			Name:        update.Metadata.GetName(),
			Description: update.Metadata.GetDescription(),
		})
	case *pb.UpdateEventRuleRequest_Actions:
		if update.Actions == nil {
			return nil, status.Error(
				codes.InvalidArgument,
				"event rule actions update is required",
			)
		}
		actions, convertErr := protobuf.EventRuleActionsFrom(update.Actions.GetActions())
		if convertErr != nil {
			return nil, status.Error(codes.InvalidArgument, convertErr.Error())
		}
		err = manager.ReplaceActions(ctx, id, actions)
	default:
		return nil, status.Error(
			codes.InvalidArgument,
			"exactly one event rule update is required",
		)
	}
	if err != nil {
		return nil, eventRuleStatusError(codes.Internal, err)
	}

	rule, err := manager.GetByID(ctx, id)
	if err != nil {
		return nil, eventRuleStatusError(codes.Internal, err)
	}

	return eventRuleResponse(rule)
}

func (rs *FlowServerImpl) EnableEventRule(
	ctx context.Context,
	req *pb.EnableEventRuleRequest,
) (*pb.EventRule, error) {
	return rs.setEventRuleEnabled(ctx, req.GetRuleId(), true)
}

func (rs *FlowServerImpl) DisableEventRule(
	ctx context.Context,
	req *pb.DisableEventRuleRequest,
) (*pb.EventRule, error) {
	return rs.setEventRuleEnabled(ctx, req.GetRuleId(), false)
}

func (rs *FlowServerImpl) setEventRuleEnabled(
	ctx context.Context,
	ruleID *pb.UUID,
	enabled bool,
) (*pb.EventRule, error) {
	manager, err := rs.requireEventRuleManager()
	if err != nil {
		return nil, err
	}

	id, err := requiredEventRuleUUID(ruleID, "event rule ID")
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}

	if err := manager.SetEnabled(ctx, id, enabled); err != nil {
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
	if err := manager.Delete(ctx, id); err != nil {
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
	scope, err := protobuf.EventRuleScopeFrom(req.GetScope())
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}

	binding, err := manager.GetBindingForScope(ctx, eventType, scope)
	if err != nil {
		return nil, eventRuleStatusError(codes.Internal, err)
	}
	if binding == nil {
		return nil, status.Error(codes.NotFound, "event rule binding was not found")
	}

	return eventRuleBindingResponse(binding)
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
	scope, err := protobuf.EventRuleScopeFrom(req.GetScope())
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}

	if err := manager.Unbind(ctx, eventType, scope); err != nil {
		return nil, eventRuleStatusError(codes.Internal, err)
	}

	return &emptypb.Empty{}, nil
}

func (rs *FlowServerImpl) SetEventRuleOverride(
	ctx context.Context,
	req *pb.SetEventRuleOverrideRequest,
) (*pb.EventRule, error) {
	manager, err := rs.requireEventRuleManager()
	if err != nil {
		return nil, err
	}

	scope, err := rs.resolveEventRuleScope(ctx, req.GetScope())
	if err != nil {
		return nil, err
	}

	actions, err := protobuf.EventRuleActionsFrom(req.GetActions())
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}

	rule, err := manager.SetOverride(
		ctx,
		eventrule.RuleCreate{
			Metadata: eventrule.RuleMetadata{
				Name:        req.GetName(),
				Description: req.GetDescription(),
			},
			EventType: eventrule.Type(req.GetEventType()),
			Policy:    eventrule.Policy{Actions: actions},
		},
		scope,
	)
	if err != nil {
		return nil, eventRuleStatusError(codes.Internal, err)
	}

	return eventRuleResponse(rule)
}

func (rs *FlowServerImpl) GetEffectiveEventRuleForScope(
	ctx context.Context,
	req *pb.GetEffectiveEventRuleForScopeRequest,
) (*pb.EventRule, error) {
	manager, err := rs.requireEventRuleManager()
	if err != nil {
		return nil, err
	}

	scope, err := rs.resolveEventRuleScope(ctx, req.GetScope())
	if err != nil {
		return nil, err
	}

	rule, err := manager.GetEffectiveForScope(
		ctx, eventrule.Type(req.GetEventType()), scope,
	)
	if err != nil {
		return nil, eventRuleStatusError(codes.Internal, err)
	}

	return eventRuleResponse(rule)
}

func (rs *FlowServerImpl) DeleteEventRuleOverride(
	ctx context.Context,
	req *pb.DeleteEventRuleOverrideRequest,
) (*emptypb.Empty, error) {
	manager, err := rs.requireEventRuleManager()
	if err != nil {
		return nil, err
	}

	scope, err := rs.resolveEventRuleScope(ctx, req.GetScope())
	if err != nil {
		return nil, err
	}

	err = manager.DeleteOverride(ctx, eventrule.Type(req.GetEventType()), scope)
	if err != nil {
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

const maxEventHandlingLimit = 100

func eventHandlingLimit(requested int32) (int, error) {
	if requested == 0 {
		return 1, nil
	}

	if requested < 1 || requested > maxEventHandlingLimit {
		return 0, fmt.Errorf(
			"limit must be between 1 and %d",
			maxEventHandlingLimit,
		)
	}

	return int(requested), nil
}

// GetEventHandling returns one cursor-paginated page of handling decisions for
// an event type and component or rack resource.
func (rs *FlowServerImpl) GetEventHandling(
	ctx context.Context,
	req *pb.GetEventHandlingRequest,
) (*pb.GetEventHandlingResponse, error) {
	manager, err := rs.requireEventRuleManager()
	if err != nil {
		return nil, err
	}

	if req.GetEventType() == "" {
		return nil, status.Error(codes.InvalidArgument, "event_type is required")
	}

	if req.GetResourceId() == "" {
		return nil, status.Error(codes.InvalidArgument, "resource_id is required")
	}

	resourceType, resourceID, err := rs.resolveEventHandlingResource(
		ctx,
		req.GetResourceType(),
		req.GetResourceId(),
	)
	if err != nil {
		return nil, err
	}

	limit, err := eventHandlingLimit(req.GetLimit())
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}

	cursor, err := eventrule.DecodeHandlingCursor(req.GetCursor())
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}

	handlings, hasMore, err := manager.GetEventHandling(
		ctx,
		eventrule.Type(req.GetEventType()),
		resourceType,
		resourceID,
		limit,
		cursor,
	)
	if err != nil {
		return nil, eventRuleStatusError(codes.Internal, err)
	}

	converted := make([]*pb.EventHandling, 0, len(handlings))
	for _, item := range handlings {
		converted = append(converted, protobuf.EventHandlingTo(item))
	}

	response := &pb.GetEventHandlingResponse{Handlings: converted}
	if hasMore && len(handlings) > 0 {
		last := handlings[len(handlings)-1]
		response.NextCursor, err = eventrule.EncodeHandlingCursor(
			eventrule.HandlingCursor{
				HandledAt: last.HandledAt,
				ID:        last.ID,
			},
		)
		if err != nil {
			return nil, status.Error(codes.Internal, err.Error())
		}
	}

	return response, nil
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
