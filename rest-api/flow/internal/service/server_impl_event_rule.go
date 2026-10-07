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
