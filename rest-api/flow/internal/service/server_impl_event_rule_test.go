// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"errors"
	"fmt"
	"math"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"

	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

var _ eventRuleManager = (*mockEventRuleManager)(nil)

func TestFlowServerImpl_CreateEventRule(t *testing.T) {
	tests := []struct {
		name        string
		manager     *mockEventRuleManager
		mutate      func(*pb.CreateEventRuleRequest)
		wantCode    codes.Code
		wantCalled  bool
		wantEnabled bool
	}{
		{name: "creates disabled rule", manager: &mockEventRuleManager{}, wantCalled: true},
		{
			name: "creates initially enabled rule atomically", manager: &mockEventRuleManager{}, wantCalled: true, wantEnabled: true,
			mutate: func(req *pb.CreateEventRuleRequest) { enabled := true; req.Enabled = &enabled },
		},
		{
			name:    "rejects invalid action at boundary",
			manager: &mockEventRuleManager{},
			mutate: func(req *pb.CreateEventRuleRequest) {
				req.Actions[0].Spec = nil
			},
			wantCode: codes.InvalidArgument,
		},
		{
			name: "maps runtime validation",
			manager: &mockEventRuleManager{
				createErr: fmt.Errorf("%w: unsupported action", eventrule.ErrInvalidRuleInput),
			},
			wantCode:   codes.InvalidArgument,
			wantCalled: true,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			server := &FlowServerImpl{eventRuleManager: test.manager}
			req := validCreateEventRuleRequest()
			if test.mutate != nil {
				test.mutate(req)
			}

			response, err := server.CreateEventRule(context.Background(), req)

			require.Equal(t, test.wantCode, status.Code(err))
			require.Equal(t, test.wantCalled, test.manager.createCalls == 1)
			if test.wantCode != codes.OK {
				require.Nil(t, response)
				return
			}
			require.Equal(t, test.manager.rule.ID.String(), response.GetId().GetId())
			require.False(t, response.GetReadOnly())
			require.Equal(t, test.wantEnabled, response.GetEnabled())
			require.Len(t, response.GetActions(), 1)
			require.NotNil(t, response.GetActions()[0].GetNoop())
		})
	}
}

func TestFlowServerImpl_GetEventRuleReturnsBuiltIn(t *testing.T) {
	manager := &mockEventRuleManager{rule: validAPIEventRule(eventrule.RuleOriginBuiltIn)}
	server := &FlowServerImpl{eventRuleManager: manager}

	response, err := server.GetEventRule(context.Background(), &pb.GetEventRuleRequest{
		RuleId: &pb.UUID{Id: manager.rule.ID.String()},
	})

	require.NoError(t, err)
	require.True(t, response.GetReadOnly())
	require.True(t, response.GetEnabled())
	require.Nil(t, response.GetCreatedAt())
	require.Nil(t, response.GetUpdatedAt())
}

func TestFlowServerImpl_GetEffectiveEventRule(t *testing.T) {
	tests := []struct {
		name     string
		request  func(uuid.UUID) *pb.GetEffectiveEventRuleRequest
		wantKind eventrule.ResourceKind
		wantCode codes.Code
	}{
		{
			name: "rack target",
			request: func(id uuid.UUID) *pb.GetEffectiveEventRuleRequest {
				return &pb.GetEffectiveEventRuleRequest{
					EventType: "hardware.leak.detected",
					Target: &pb.GetEffectiveEventRuleRequest_RackId{
						RackId: &pb.UUID{Id: id.String()},
					},
				}
			},
			wantKind: eventrule.ResourceKindRack,
		},
		{
			name: "component target",
			request: func(id uuid.UUID) *pb.GetEffectiveEventRuleRequest {
				return &pb.GetEffectiveEventRuleRequest{
					EventType: "hardware.leak.detected",
					Target: &pb.GetEffectiveEventRuleRequest_ComponentId{
						ComponentId: &pb.UUID{Id: id.String()},
					},
				}
			},
			wantKind: eventrule.ResourceKindComponent,
		},
		{
			name: "missing target",
			request: func(uuid.UUID) *pb.GetEffectiveEventRuleRequest {
				return &pb.GetEffectiveEventRuleRequest{EventType: "hardware.leak.detected"}
			},
			wantCode: codes.InvalidArgument,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			targetID := uuid.New()
			manager := &mockEventRuleManager{
				effectiveRule: validAPIEventRule(eventrule.RuleOriginBuiltIn),
			}
			server := &FlowServerImpl{eventRuleManager: manager}

			response, err := server.GetEffectiveEventRule(
				context.Background(),
				test.request(targetID),
			)

			require.Equal(t, test.wantCode, status.Code(err))
			if test.wantCode != codes.OK {
				require.Nil(t, response)
				return
			}
			require.True(t, response.GetReadOnly())
			require.Equal(t, test.wantKind, manager.effectiveTarget.Kind)
			require.Equal(t, targetID, manager.effectiveTarget.ID)
		})
	}
}

func TestFlowServerImpl_ListEventRules(t *testing.T) {
	tests := map[string]struct {
		pagination *pb.Pagination
		page       eventrule.RuleListPage
		wantCode   codes.Code
		wantOffset int
		wantLimit  int
	}{
		"returns page and total": {
			pagination: &pb.Pagination{Offset: 5, Limit: 10},
			page: eventrule.RuleListPage{
				Rules: []*eventrule.Rule{validAPIEventRule(eventrule.RuleOriginPersisted)},
				Total: 42,
			},
			wantOffset: 5,
			wantLimit:  10,
		},
		"uses default pagination": {
			page:      eventrule.RuleListPage{Total: 1},
			wantLimit: dbquery.DefaultPaginationLimit,
		},
		"returns total beyond int32": {
			page:      eventrule.RuleListPage{Total: math.MaxInt32 + 1},
			wantLimit: dbquery.DefaultPaginationLimit,
		},
		"rejects invalid pagination": {
			pagination: &pb.Pagination{},
			wantCode:   codes.InvalidArgument,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			manager := &mockEventRuleManager{listPage: test.page}
			server := &FlowServerImpl{eventRuleManager: manager}

			response, err := server.ListEventRules(
				context.Background(),
				&pb.ListEventRulesRequest{Pagination: test.pagination},
			)

			require.Equal(t, test.wantCode, status.Code(err))
			if test.wantCode != codes.OK {
				require.Nil(t, response)
				require.Zero(t, manager.listCalls)
				return
			}

			require.EqualValues(t, test.page.Total, response.GetTotal())
			require.Len(t, response.GetRules(), len(test.page.Rules))
			require.Equal(t, 1, manager.listCalls)
			require.Equal(t, test.wantOffset, manager.listRequest.Offset)
			require.Equal(t, test.wantLimit, manager.listRequest.Limit)
		})
	}
}

func TestFlowServerImpl_SetEventRuleEnabled(t *testing.T) {
	id := uuid.New()
	for _, test := range []struct {
		name       string
		ruleID     *pb.UUID
		enabled    *bool
		managerErr error
		code       codes.Code
		wantCalls  int
	}{
		{name: "enable", ruleID: &pb.UUID{Id: id.String()}, enabled: proto.Bool(true), wantCalls: 1},
		{name: "disable", ruleID: &pb.UUID{Id: id.String()}, enabled: proto.Bool(false), wantCalls: 1},
		{name: "missing state", ruleID: &pb.UUID{Id: id.String()}, code: codes.InvalidArgument},
		{name: "missing ID", enabled: proto.Bool(false), code: codes.InvalidArgument},
		{name: "invalid ID", ruleID: &pb.UUID{Id: "invalid"}, enabled: proto.Bool(true), code: codes.InvalidArgument},
		{name: "read-only rule", ruleID: &pb.UUID{Id: id.String()}, enabled: proto.Bool(true), managerErr: eventrule.ErrBuiltInRuleImmutable, code: codes.FailedPrecondition, wantCalls: 1},
		{name: "unknown rule", ruleID: &pb.UUID{Id: id.String()}, enabled: proto.Bool(false), managerErr: eventrule.ErrRuleNotFound, code: codes.NotFound, wantCalls: 1},
		{name: "storage failure", ruleID: &pb.UUID{Id: id.String()}, enabled: proto.Bool(true), managerErr: errors.New("store unavailable"), code: codes.Internal, wantCalls: 1},
	} {
		t.Run(test.name, func(t *testing.T) {
			manager := &mockEventRuleManager{setEnabledErr: test.managerErr, rule: validAPIEventRule(eventrule.RuleOriginPersisted)}
			if test.enabled != nil {
				manager.rule.Enabled = !*test.enabled
			}
			server := &FlowServerImpl{eventRuleManager: manager}
			response, err := server.SetEventRuleEnabled(t.Context(), &pb.SetEventRuleEnabledRequest{RuleId: test.ruleID, Enabled: test.enabled})
			require.Equal(t, test.code, status.Code(err))
			require.Equal(t, test.wantCalls, manager.setEnabledCalls)
			if test.wantCalls > 0 {
				require.Equal(t, id, manager.setEnabledID)
			}
			if test.code != codes.OK {
				require.Nil(t, response)
				return
			}
			require.Equal(t, *test.enabled, response.GetEnabled())
		})
	}
}

func TestFlowServerImpl_DeleteEventRule(t *testing.T) {
	id := uuid.New()
	for _, test := range []struct {
		name       string
		ruleID     *pb.UUID
		managerErr error
		code       codes.Code
	}{
		{name: "unbound policy", ruleID: &pb.UUID{Id: id.String()}},
		{name: "missing policy ID", code: codes.InvalidArgument},
		{name: "invalid policy ID", ruleID: &pb.UUID{Id: "invalid"}, code: codes.InvalidArgument},
		{name: "bound policy", ruleID: &pb.UUID{Id: id.String()}, managerErr: fmt.Errorf("%w: remaining assignment", eventrule.ErrBindingConflict), code: codes.AlreadyExists},
		{name: "unknown policy", ruleID: &pb.UUID{Id: id.String()}, managerErr: eventrule.ErrRuleNotFound, code: codes.NotFound},
		{name: "built-in policy", ruleID: &pb.UUID{Id: id.String()}, managerErr: eventrule.ErrBuiltInRuleImmutable, code: codes.FailedPrecondition},
		{name: "storage failure", ruleID: &pb.UUID{Id: id.String()}, managerErr: fmt.Errorf("storage failure"), code: codes.Internal},
	} {
		t.Run(test.name, func(t *testing.T) {
			manager := &mockEventRuleManager{deleteErr: test.managerErr}
			server := &FlowServerImpl{eventRuleManager: manager}
			response, err := server.DeleteEventRule(t.Context(), &pb.DeleteEventRuleRequest{RuleId: test.ruleID})
			require.Equal(t, test.code, status.Code(err))
			if test.code == codes.InvalidArgument {
				require.Equal(t, uuid.Nil, manager.deletedRuleID)
			} else {
				require.Equal(t, id, manager.deletedRuleID)
			}
			if test.code != codes.OK {
				require.Nil(t, response)
				return
			}
			require.NotNil(t, response)
		})
	}
}

func TestFlowServerImpl_CreateEventRuleBinding(t *testing.T) {
	tests := map[string]struct {
		bindErr  error
		wantCode codes.Code
	}{
		"creates binding": {
			wantCode: codes.OK,
		},
		"invalid rack scope": {
			bindErr: fmt.Errorf(
				"%w: rack does not exist",
				eventrule.ErrInvalidRuleInput,
			),
			wantCode: codes.InvalidArgument,
		},
		"hierarchy conflict": {
			bindErr:  fmt.Errorf("%w: mixed scope", eventrule.ErrBindingConflict),
			wantCode: codes.FailedPrecondition,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			manager := &mockEventRuleManager{bindErr: test.bindErr}
			server := &FlowServerImpl{eventRuleManager: manager}
			rule := manager.ensureRule()

			response, err := server.CreateEventRuleBinding(
				context.Background(),
				&pb.CreateEventRuleBindingRequest{
					RuleId: &pb.UUID{Id: rule.ID.String()},
					Scope: &pb.EventRuleScope{
						Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE,
					},
				},
			)

			require.Equal(t, test.wantCode, status.Code(err))
			if test.wantCode != codes.OK {
				require.Nil(t, response)
				return
			}
			require.NotNil(t, response)
		})
	}
}

func TestFlowServerImpl_GetEventRuleBinding(t *testing.T) {
	for _, test := range []struct {
		name     string
		request  *pb.GetEventRuleBindingRequest
		wantCode codes.Code
	}{
		{name: "absent Site binding", request: &pb.GetEventRuleBindingRequest{EventType: "hardware.leak.detected", Scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE}}, wantCode: codes.NotFound},
		{name: "unknown public Rack", request: &pb.GetEventRuleBindingRequest{EventType: "hardware.leak.detected", Selector: &pb.EventRuleScopeSelector{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, RackIdentifier: &pb.EventRuleScopeSelector_RackExternalId{RackExternalId: "missing-rack"}}}, wantCode: codes.NotFound},
		{name: "invalid canonical scope", request: &pb.GetEventRuleBindingRequest{EventType: "hardware.leak.detected", Scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK}}, wantCode: codes.InvalidArgument},
	} {
		t.Run(test.name, func(t *testing.T) {
			server := &FlowServerImpl{eventRuleManager: &mockEventRuleManager{}, inventoryManager: newMockManager()}
			response, err := server.GetEventRuleBinding(t.Context(), test.request)
			require.Nil(t, response)
			require.Equal(t, test.wantCode, status.Code(err))
		})
	}
}

func TestFlowServerImpl_ListEventRuleBindings(t *testing.T) {
	rackID, policyID := uuid.New(), uuid.New()
	uuidExternalID := uuid.NewString()
	for _, test := range []struct {
		name        string
		mutate      func(*pb.ListEventRuleBindingsRequest)
		wantType    eventrule.Type
		wantScope   eventrule.ScopeType
		wantRack    uuid.UUID
		wantPolicy  uuid.UUID
		bad         bool
		missingRack bool
		externalID  string
	}{
		{name: "all assignments"},
		{name: "event type", mutate: func(r *pb.ListEventRuleBindingsRequest) { v := "hardware.leak.detected"; r.EventType = &v }, wantType: "hardware.leak.detected"},
		{name: "Rack scope", mutate: func(r *pb.ListEventRuleBindingsRequest) {
			r.ScopeType = pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK
		}, wantScope: eventrule.ScopeTypeRack},
		{name: "UUID-shaped external ID matching another Rack UUID", mutate: func(r *pb.ListEventRuleBindingsRequest) { r.RackId = &uuidExternalID }, externalID: uuidExternalID, wantRack: rackID},
		{name: "public Rack", mutate: func(r *pb.ListEventRuleBindingsRequest) { v := "rack-17"; r.RackId = &v }, wantRack: rackID},
		{name: "unknown public Rack", mutate: func(r *pb.ListEventRuleBindingsRequest) { v := "missing-rack"; r.RackId = &v }, missingRack: true},
		{name: "policy filter", mutate: func(r *pb.ListEventRuleBindingsRequest) { r.RuleId = &pb.UUID{Id: policyID.String()} }, wantPolicy: policyID},
		{name: "empty event type", mutate: func(r *pb.ListEventRuleBindingsRequest) { v := ""; r.EventType = &v }, bad: true},
		{name: "unknown scope", mutate: func(r *pb.ListEventRuleBindingsRequest) { r.ScopeType = pb.EventRuleScopeType(99) }, bad: true},
		{name: "invalid policy UUID", mutate: func(r *pb.ListEventRuleBindingsRequest) { r.RuleId = &pb.UUID{Id: "bad"} }, bad: true},
		{name: "Site and Rack", mutate: func(r *pb.ListEventRuleBindingsRequest) {
			r.ScopeType = pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE
			v := "rack-17"
			r.RackId = &v
		}, bad: true},
		{name: "negative offset", mutate: func(r *pb.ListEventRuleBindingsRequest) { r.Pagination.Offset = -1 }, bad: true},
	} {
		t.Run(test.name, func(t *testing.T) {
			manager := &mockEventRuleManager{}
			inventory := newMockManager()
			inventory.racks[rackID] = &rack.Rack{Info: deviceinfo.DeviceInfo{ID: rackID}, ExternalID: "rack-17"}
			if test.externalID != "" {
				inventory.racks[rackID].ExternalID = test.externalID
				otherID := uuid.MustParse(test.externalID)
				inventory.racks[otherID] = &rack.Rack{Info: deviceinfo.DeviceInfo{ID: otherID}, ExternalID: "other-rack"}
			}
			server := &FlowServerImpl{eventRuleManager: manager, inventoryManager: inventory}
			request := &pb.ListEventRuleBindingsRequest{Pagination: &pb.Pagination{Offset: 2, Limit: 3}}
			if test.mutate != nil {
				test.mutate(request)
			}
			response, err := server.ListEventRuleBindings(t.Context(), request)
			if test.bad {
				require.Equal(t, codes.InvalidArgument, status.Code(err))
				return
			}
			require.NoError(t, err)
			require.Empty(t, response.GetBindings())
			if test.missingRack {
				require.Equal(t, eventrule.BindingListRequest{}, manager.bindingQuery)
				return
			}
			require.Equal(t, eventrule.BindingListRequest{EventType: test.wantType, RuleID: test.wantPolicy, ScopeType: test.wantScope, RackID: test.wantRack, Offset: 2, Limit: 3}, manager.bindingQuery)
		})
	}
}

func TestFlowServerImpl_PutEventRuleBinding(t *testing.T) {
	for _, test := range []struct {
		name       string
		created    bool
		rackScope  bool
		managerErr error
		wantCode   codes.Code
	}{
		{name: "assign Site", created: true},
		{name: "resolve public Rack and return public identifier", rackScope: true},
		{name: "missing body policy is invalid input", managerErr: eventrule.ErrRuleNotFound, wantCode: codes.InvalidArgument},
		{name: "policy type mismatch is invalid input", managerErr: eventrule.ErrInvalidRuleInput, wantCode: codes.InvalidArgument},
	} {
		t.Run(test.name, func(t *testing.T) {
			policyID, rackID := uuid.New(), uuid.New()
			manager := &mockEventRuleManager{bindErr: test.managerErr, bindingCreated: test.created}
			inventory := newMockManager()
			inventory.racks[rackID] = &rack.Rack{Info: deviceinfo.DeviceInfo{ID: rackID}, ExternalID: "rack-17"}
			server := &FlowServerImpl{eventRuleManager: manager, inventoryManager: inventory}
			scope := &pb.EventRuleScopeSelector{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE}
			if test.rackScope {
				scope.Type = pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK
				scope.RackIdentifier = &pb.EventRuleScopeSelector_RackExternalId{RackExternalId: "rack-17"}
			}
			response, err := server.PutEventRuleBinding(t.Context(), &pb.PutEventRuleBindingRequest{RuleId: &pb.UUID{Id: policyID.String()}, EventType: "hardware.leak.detected", Scope: scope})
			require.Equal(t, test.wantCode, status.Code(err))
			if test.wantCode != codes.OK {
				require.Nil(t, response)
				return
			}
			require.Equal(t, test.created, response.GetCreated())
			require.Equal(t, policyID.String(), response.GetBinding().GetRuleId().GetId())
			require.Equal(t, "hardware.leak.detected", response.GetBinding().GetEventType())
			if test.rackScope {
				require.Equal(t, rackID, manager.binding.Scope.ID)
				require.Equal(t, "rack-17", response.GetBinding().GetRackExternalId())
			} else {
				require.Equal(t, eventrule.Scope{Type: eventrule.ScopeTypeSite}, manager.binding.Scope)
			}
		})
	}
}

func TestFlowServerImpl_DeleteEventRuleBinding(t *testing.T) {
	eventType := eventrule.Type("hardware.leak.detected")
	siteScope := eventrule.Scope{Type: eventrule.ScopeTypeSite}
	validRequest := func() *pb.DeleteEventRuleBindingRequest {
		return &pb.DeleteEventRuleBindingRequest{
			EventType: string(eventType),
			Scope: &pb.EventRuleScope{
				Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE,
			},
		}
	}

	tests := map[string]struct {
		request   *pb.DeleteEventRuleBindingRequest
		unbindErr error
		wantCode  codes.Code
	}{
		"deletes resolution slot": {
			request:  validRequest(),
			wantCode: codes.OK,
		},
		"invalid event type": {
			request: &pb.DeleteEventRuleBindingRequest{
				EventType: "Invalid",
				Scope:     validRequest().Scope,
			},
			wantCode: codes.InvalidArgument,
		},
		"missing scope": {
			request: &pb.DeleteEventRuleBindingRequest{
				EventType: string(eventType),
			},
			wantCode: codes.InvalidArgument,
		},
		"unknown public Rack": {
			request: &pb.DeleteEventRuleBindingRequest{
				EventType: string(eventType),
				Selector:  &pb.EventRuleScopeSelector{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, RackIdentifier: &pb.EventRuleScopeSelector_RackExternalId{RackExternalId: "missing-rack"}},
			},
			wantCode: codes.NotFound,
		},
		"binding not found": {
			request:   validRequest(),
			unbindErr: fmt.Errorf("%w: missing binding", eventrule.ErrBindingNotFound),
			wantCode:  codes.NotFound,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			manager := &mockEventRuleManager{unbindErr: test.unbindErr}
			server := &FlowServerImpl{eventRuleManager: manager, inventoryManager: newMockManager()}

			response, err := server.DeleteEventRuleBinding(
				context.Background(),
				test.request,
			)

			require.Equal(t, test.wantCode, status.Code(err))
			if test.wantCode != codes.OK {
				require.Nil(t, response)
				return
			}

			require.NotNil(t, response)
			require.Equal(t, eventType, manager.unboundEventType)
			require.Equal(t, siteScope, manager.unboundScope)
		})
	}
}

func TestFlowServerImpl_resolveEventRuleScope(t *testing.T) {
	rackID := uuid.New()
	inventoryManager := newMockManager()
	inventoryManager.racks[rackID] = &rack.Rack{
		Info:       deviceinfo.DeviceInfo{ID: rackID},
		ExternalID: "rack-external-1",
	}
	server := &FlowServerImpl{inventoryManager: inventoryManager}

	tests := []struct {
		name      string
		selector  *pb.EventRuleScopeSelector
		wantScope eventrule.Scope
		wantCode  codes.Code
	}{
		{
			name: "site scope",
			selector: &pb.EventRuleScopeSelector{
				Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE,
			},
			wantScope: eventrule.Scope{Type: eventrule.ScopeTypeSite},
		},
		{
			name: "rack Flow UUID",
			selector: &pb.EventRuleScopeSelector{
				Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK,
				RackIdentifier: &pb.EventRuleScopeSelector_RackId{
					RackId: &pb.UUID{Id: rackID.String()},
				},
			},
			wantScope: eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: rackID},
		},
		{
			name: "rack external ID",
			selector: &pb.EventRuleScopeSelector{
				Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK,
				RackIdentifier: &pb.EventRuleScopeSelector_RackExternalId{
					RackExternalId: "rack-external-1",
				},
			},
			wantScope: eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: rackID},
		},
		{
			name:     "missing selector",
			wantCode: codes.InvalidArgument,
		},
		{
			name: "site with rack identifier",
			selector: &pb.EventRuleScopeSelector{
				Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE,
				RackIdentifier: &pb.EventRuleScopeSelector_RackExternalId{
					RackExternalId: "rack-external-1",
				},
			},
			wantCode: codes.InvalidArgument,
		},
		{
			name: "rack without identifier",
			selector: &pb.EventRuleScopeSelector{
				Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK,
			},
			wantCode: codes.InvalidArgument,
		},
		{
			name: "invalid rack Flow UUID",
			selector: &pb.EventRuleScopeSelector{
				Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK,
				RackIdentifier: &pb.EventRuleScopeSelector_RackId{
					RackId: &pb.UUID{Id: "not-a-uuid"},
				},
			},
			wantCode: codes.InvalidArgument,
		},
		{
			name: "unknown rack external ID",
			selector: &pb.EventRuleScopeSelector{
				Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK,
				RackIdentifier: &pb.EventRuleScopeSelector_RackExternalId{
					RackExternalId: "missing-rack",
				},
			},
			wantCode: codes.NotFound,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			got, err := server.resolveEventRuleScope(t.Context(), test.selector)
			require.Equal(t, test.wantCode, status.Code(err))
			if test.wantCode != codes.OK {
				return
			}
			require.Equal(t, test.wantScope, got)
		})
	}
}

type mockEventRuleManager struct {
	bindingQuery   eventrule.BindingListRequest
	bindingCreated bool
	query          eventrule.HandlingQuery
	eventRuleManager
	rule             *eventrule.Rule
	createCalls      int
	createErr        error
	setEnabledCalls  int
	setEnabledID     uuid.UUID
	setEnabledErr    error
	bindErr          error
	unbindErr        error
	binding          *eventrule.Binding
	effectiveRule    *eventrule.Rule
	effectiveTarget  eventrule.ResourceIdentity
	listPage         eventrule.RuleListPage
	listRequest      eventrule.RuleListRequest
	listCalls        int
	unboundEventType eventrule.Type
	unboundScope     eventrule.Scope
	handling         []eventrule.Handling
	handlingTotal    int
	handlingErr      error
	deleteErr        error
	deletedRuleID    uuid.UUID
}

func (m *mockEventRuleManager) ListBindings(_ context.Context, query eventrule.BindingListRequest) ([]eventrule.Binding, int, error) {
	m.bindingQuery = query
	return []eventrule.Binding{}, 0, nil
}

func (m *mockEventRuleManager) PutBinding(_ context.Context, eventType eventrule.Type, id uuid.UUID, scope eventrule.Scope) (*eventrule.BindingPutResult, error) {
	if m.bindErr != nil {
		return nil, m.bindErr
	}
	m.binding = &eventrule.Binding{ID: uuid.New(), RuleID: id, EventType: eventType, Scope: scope}
	return &eventrule.BindingPutResult{Binding: m.binding, Created: m.bindingCreated}, nil
}

func (m *mockEventRuleManager) ensureRule() *eventrule.Rule {
	if m.rule == nil {
		m.rule = validAPIEventRule(eventrule.RuleOriginPersisted)
	}
	return m.rule
}

func (m *mockEventRuleManager) Create(
	_ context.Context,
	input eventrule.RuleCreate,
) (*eventrule.Rule, error) {
	m.createCalls++
	if m.createErr != nil {
		return nil, m.createErr
	}
	rule := m.ensureRule()
	rule.Name = input.Metadata.Name
	rule.Description = input.Metadata.Description
	rule.EventType = input.EventType
	rule.Enabled = input.Enabled
	rule.Policy = input.Policy
	cloned := rule.Clone()
	return &cloned, nil
}

func (m *mockEventRuleManager) GetByID(context.Context, uuid.UUID) (*eventrule.Rule, error) {
	rule := m.ensureRule()
	cloned := rule.Clone()
	return &cloned, nil
}

func (m *mockEventRuleManager) GetEffective(
	_ context.Context,
	_ eventrule.Type,
	target eventrule.ResourceIdentity,
) (*eventrule.Rule, error) {
	m.effectiveTarget = target
	if m.effectiveRule == nil {
		return nil, nil
	}
	cloned := m.effectiveRule.Clone()
	return &cloned, nil
}

func (m *mockEventRuleManager) List(
	_ context.Context,
	request eventrule.RuleListRequest,
) (eventrule.RuleListPage, error) {
	m.listCalls++
	m.listRequest = request
	return m.listPage, nil
}

func (m *mockEventRuleManager) SetEnabled(_ context.Context, id uuid.UUID, enabled bool) error {
	m.setEnabledCalls++
	m.setEnabledID = id
	if m.setEnabledErr != nil {
		return m.setEnabledErr
	}
	m.ensureRule().Enabled = enabled
	return nil
}

func (m *mockEventRuleManager) Delete(_ context.Context, id uuid.UUID) error {
	m.deletedRuleID = id
	return m.deleteErr
}

func (m *mockEventRuleManager) Bind(
	_ context.Context,
	ruleID uuid.UUID,
	scope eventrule.Scope,
) (*eventrule.Binding, error) {
	if m.bindErr != nil {
		return nil, m.bindErr
	}
	return &eventrule.Binding{
		ID:        uuid.New(),
		RuleID:    ruleID,
		EventType: m.ensureRule().EventType,
		Scope:     scope,
	}, nil
}

func (m *mockEventRuleManager) GetBindingForScope(
	context.Context,
	eventrule.Type,
	eventrule.Scope,
) (*eventrule.Binding, error) {
	return m.binding, nil
}

func (m *mockEventRuleManager) Unbind(
	_ context.Context,
	eventType eventrule.Type,
	scope eventrule.Scope,
) error {
	m.unboundEventType = eventType
	m.unboundScope = scope

	return m.unbindErr
}

func validCreateEventRuleRequest() *pb.CreateEventRuleRequest {
	return &pb.CreateEventRuleRequest{
		Name:      "leak response",
		EventType: "hardware.leak.detected",
		Actions: []*pb.EventRuleAction{{
			Name: "audit",
			Spec: &pb.EventRuleAction_Noop{
				Noop: &pb.EventRuleNoopAction{Reason: "test"},
			},
		}},
	}
}

func validAPIEventRule(origin eventrule.RuleOrigin) *eventrule.Rule {
	now := time.Date(2026, 8, 31, 12, 0, 0, 0, time.UTC)
	rule := &eventrule.Rule{
		ID:        uuid.New(),
		Origin:    origin,
		Name:      "leak response",
		Enabled:   origin == eventrule.RuleOriginBuiltIn,
		EventType: "hardware.leak.detected",
		Policy: eventrule.Policy{
			Actions: []eventrule.Action{{
				Name: "audit",
				Spec: &eventrule.Noop{Reason: "test"},
			}},
		},
	}
	if origin == eventrule.RuleOriginPersisted {
		rule.CreatedAt = now
		rule.UpdatedAt = now
	}
	return rule
}

func (m *mockEventRuleManager) ValidateEventType(value eventrule.Type) error {
	if value != "hardware.leak.detected" {
		return fmt.Errorf("%w: unsupported event type", eventrule.ErrInvalidRuleInput)
	}
	return nil
}
func (m *mockEventRuleManager) ListEventHandlings(_ context.Context, query eventrule.HandlingQuery) ([]eventrule.Handling, int, error) {
	m.query = query
	return m.handling, m.handlingTotal, m.handlingErr
}

func TestFlowServerImpl_ListEventHandlings(t *testing.T) {
	eventType, empty, unknown := "hardware.leak.detected", "", "other.event"
	rackID, componentID, policyID, eventID := uuid.New(), uuid.New(), uuid.New(), uuid.New()
	publicRack, publicComponent := "rack-17", "component-17"
	uuidExternalID := uuid.NewString()
	handledAt := time.Date(2026, 10, 26, 16, 0, 0, 0, time.UTC)
	for _, tc := range []struct {
		name          string
		filter        *pb.EventHandlingFilter
		page          *pb.Pagination
		code          codes.Code
		managerErr    error
		invalidPolicy bool
		externalID    string
		wantQuery     eventrule.HandlingQuery
	}{
		{name: "all event types", wantQuery: eventrule.HandlingQuery{Limit: dbquery.DefaultPaginationLimit}},
		{name: "UUID-shaped external ID matching another Rack UUID", filter: &pb.EventHandlingFilter{RackId: &uuidExternalID}, externalID: uuidExternalID, wantQuery: eventrule.HandlingQuery{RackID: rackID, Limit: dbquery.DefaultPaginationLimit}},
		{name: "combined captured filters", filter: &pb.EventHandlingFilter{EventType: &eventType, PolicyId: &pb.UUID{Id: policyID.String()}, RackId: &publicRack}, page: &pb.Pagination{Offset: 2, Limit: 5}, wantQuery: eventrule.HandlingQuery{EventType: eventrule.Type(eventType), PolicyID: policyID, RackID: rackID, Offset: 2, Limit: 5}},
		{name: "component snapshot and offset", filter: &pb.EventHandlingFilter{EventType: &eventType, ResourceType: pb.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT, ResourceId: &publicComponent}, page: &pb.Pagination{Offset: 5, Limit: 20}, wantQuery: eventrule.HandlingQuery{EventType: eventrule.Type(eventType), ResourceType: eventrule.ResourceKindComponent, ResourceID: componentID, Offset: 5, Limit: 20}},
		{name: "Rack source", filter: &pb.EventHandlingFilter{EventType: &eventType, ResourceType: pb.EventResourceType_EVENT_RESOURCE_TYPE_RACK, ResourceId: &publicRack}, wantQuery: eventrule.HandlingQuery{EventType: eventrule.Type(eventType), ResourceType: eventrule.ResourceKindRack, ResourceID: rackID, Limit: dbquery.DefaultPaginationLimit}},
		{name: "empty type", filter: &pb.EventHandlingFilter{EventType: &empty}, code: codes.InvalidArgument},
		{name: "unregistered type", filter: &pb.EventHandlingFilter{EventType: &unknown}, code: codes.InvalidArgument},
		{name: "empty Rack", filter: &pb.EventHandlingFilter{RackId: &empty}, code: codes.InvalidArgument},
		{name: "unpaired source", filter: &pb.EventHandlingFilter{ResourceId: &publicRack}, code: codes.InvalidArgument},
		{name: "invalid policy UUID", filter: &pb.EventHandlingFilter{PolicyId: &pb.UUID{Id: "invalid"}}, code: codes.InvalidArgument},
		{name: "negative offset", page: &pb.Pagination{Offset: -1, Limit: 1}, code: codes.InvalidArgument},
		{name: "excessive limit", page: &pb.Pagination{Limit: maxEventHandlingLimit + 1}, code: codes.InvalidArgument},
		{name: "invalid manager input", managerErr: fmt.Errorf("%w: unsupported event type", eventrule.ErrInvalidRuleInput), code: codes.InvalidArgument},
		{name: "invalid stored action", invalidPolicy: true, code: codes.Internal},
	} {
		t.Run(tc.name, func(t *testing.T) {
			inventory := newMockManager()
			inventory.racks[rackID] = &rack.Rack{Info: deviceinfo.DeviceInfo{ID: rackID}, ExternalID: publicRack}
			if tc.externalID != "" {
				inventory.racks[rackID].ExternalID = tc.externalID
				otherID := uuid.MustParse(tc.externalID)
				inventory.racks[otherID] = &rack.Rack{Info: deviceinfo.DeviceInfo{ID: otherID}, ExternalID: "other-rack"}
			}
			inventory.components[componentID] = &component.Component{Info: deviceinfo.DeviceInfo{ID: componentID}, ComponentID: publicComponent}
			policy := eventrule.Policy{Actions: []eventrule.Action{{Name: "record", Spec: &eventrule.Noop{Reason: "record leakage"}}}}
			if tc.invalidPolicy {
				policy.Actions[0].Spec = &eventrule.SubmitTask{}
			}
			resourceID, resourceType := publicComponent, eventrule.ResourceKindComponent
			if tc.wantQuery.ResourceType == eventrule.ResourceKindRack {
				resourceID, resourceType = publicRack, eventrule.ResourceKindRack
			}
			manager := &mockEventRuleManager{handlingErr: tc.managerErr, handlingTotal: 23, handling: []eventrule.Handling{{
				ID: eventID, EventType: eventrule.Type(eventType), EventSummary: "Leak detected", AppliedPolicy: policy,
				Executions: []eventrule.HandlingExecution{{ID: eventID, ActionName: "record", Status: eventrule.ExecutionStatusDeferred, Reason: eventrule.ExecutionReasonAttemptInterrupted}},
				ResourceID: resourceID, ResourceType: resourceType, HandledAt: handledAt,
			}}}
			server := &FlowServerImpl{eventRuleManager: manager, inventoryManager: inventory}
			response, err := server.ListEventHandlings(t.Context(), &pb.ListEventHandlingsRequest{Filter: tc.filter, Pagination: tc.page})
			require.Equal(t, tc.code, status.Code(err))
			if tc.code != codes.OK {
				return
			}
			require.Equal(t, tc.wantQuery, manager.query)
			require.Len(t, response.GetHandlings(), 1)
			item := response.GetHandlings()[0]
			require.Equal(t, eventID.String(), item.GetId().GetId())
			require.Equal(t, "Leak detected", item.GetEventSummary())
			require.Equal(t, resourceID, item.GetResourceId())
			require.Equal(t, pb.EventHandlingStatus_EVENT_HANDLING_STATUS_RETRYING, item.GetHandlingStatus())
			require.Equal(t, pb.EventHandlingReason_EVENT_HANDLING_REASON_ATTEMPT_INTERRUPTED, item.GetReason())
			require.Equal(t, "A handling attempt was interrupted; another attempt is scheduled.", item.GetMessage())
			require.Equal(t, "record leakage", item.GetAppliedPolicy().GetActions()[0].GetNoop().GetReason())
			require.EqualValues(t, 23, response.GetTotalCount())
		})
	}
}
