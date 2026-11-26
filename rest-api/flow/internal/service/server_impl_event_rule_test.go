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
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/encoding/protojson"
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
			require.Zero(t, test.manager.setEnabledCalls, "creation must pass the initial state directly to Create")
			if test.wantCalled {
				require.Equal(t, test.wantEnabled, test.manager.createInput.Enabled)
			}
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
		laterState *bool
	}{
		{name: "enable", ruleID: &pb.UUID{Id: id.String()}, enabled: proto.Bool(true), wantCalls: 1},
		{name: "disable", ruleID: &pb.UUID{Id: id.String()}, enabled: proto.Bool(false), wantCalls: 1},
		{name: "later mutation preserves this response", ruleID: &pb.UUID{Id: id.String()}, enabled: proto.Bool(true), wantCalls: 1, laterState: proto.Bool(false)},
		{name: "missing state", ruleID: &pb.UUID{Id: id.String()}, code: codes.InvalidArgument},
		{name: "missing ID", enabled: proto.Bool(false), code: codes.InvalidArgument},
		{name: "invalid ID", ruleID: &pb.UUID{Id: "invalid"}, enabled: proto.Bool(true), code: codes.InvalidArgument},
		{name: "read-only rule", ruleID: &pb.UUID{Id: id.String()}, enabled: proto.Bool(true), managerErr: eventrule.ErrBuiltInRuleImmutable, code: codes.FailedPrecondition, wantCalls: 1},
		{name: "unknown rule", ruleID: &pb.UUID{Id: id.String()}, enabled: proto.Bool(false), managerErr: eventrule.ErrRuleNotFound, code: codes.NotFound, wantCalls: 1},
		{name: "storage failure", ruleID: &pb.UUID{Id: id.String()}, enabled: proto.Bool(true), managerErr: errors.New("store unavailable"), code: codes.Internal, wantCalls: 1},
	} {
		t.Run(test.name, func(t *testing.T) {
			manager := &mockEventRuleManager{setEnabledErr: test.managerErr, rule: validAPIEventRule(eventrule.RuleOriginPersisted), laterEnabled: test.laterState}
			if test.enabled != nil {
				manager.rule.Enabled = !*test.enabled
			}
			server := &FlowServerImpl{eventRuleManager: manager}
			response, err := server.SetEventRuleEnabled(t.Context(), &pb.SetEventRuleEnabledRequest{RuleId: test.ruleID, Enabled: test.enabled})
			require.Equal(t, test.code, status.Code(err))
			require.Equal(t, test.wantCalls, manager.setEnabledCalls)
			require.Zero(t, manager.getByIDCalls, "return the mutation snapshot without a separate read")
			if test.wantCalls > 0 {
				require.Equal(t, id, manager.setEnabledID)
			}
			if test.code != codes.OK {
				require.Nil(t, response)
				return
			}
			require.Equal(t, *test.enabled, response.GetEnabled())
			if test.laterState != nil {
				require.Equal(t, *test.laterState, manager.rule.Enabled)
			}
		})
	}
}

func TestFlowServerImpl_EnableEventRule(t *testing.T) {
	testEventRuleStateAlias(t, true, func(ctx context.Context, client pb.FlowClient, id *pb.UUID) (*pb.EventRule, error) {
		return client.EnableEventRule(ctx, &pb.EnableEventRuleRequest{RuleId: id}) //nolint:staticcheck // Exercise the deprecated v1 method path.
	})
}

func TestFlowServerImpl_DisableEventRule(t *testing.T) {
	testEventRuleStateAlias(t, false, func(ctx context.Context, client pb.FlowClient, id *pb.UUID) (*pb.EventRule, error) {
		return client.DisableEventRule(ctx, &pb.DisableEventRuleRequest{RuleId: id}) //nolint:staticcheck // Exercise the deprecated v1 method path.
	})
}

func testEventRuleStateAlias(
	t *testing.T,
	enabled bool,
	invoke func(context.Context, pb.FlowClient, *pb.UUID) (*pb.EventRule, error),
) {
	t.Helper()
	id := uuid.New()
	for _, test := range []struct {
		name       string
		managerErr error
		code       codes.Code
	}{
		{name: "changes state through the legacy RPC"},
		{name: "forwards manager error", managerErr: eventrule.ErrRuleNotFound, code: codes.NotFound},
	} {
		t.Run(test.name, func(t *testing.T) {
			manager := &mockEventRuleManager{setEnabledErr: test.managerErr, rule: validAPIEventRule(eventrule.RuleOriginPersisted)}
			manager.rule.Enabled = !enabled
			client := flowRPCClient(t, &FlowServerImpl{eventRuleManager: manager})
			response, err := invoke(t.Context(), client, &pb.UUID{Id: id.String()})
			require.Equal(t, test.code, status.Code(err))
			require.Equal(t, 1, manager.setEnabledCalls)
			require.Equal(t, id, manager.setEnabledID)
			require.Zero(t, manager.getByIDCalls, "return the mutation snapshot without a separate read")
			if test.code != codes.OK {
				require.Nil(t, response)
				return
			}
			require.Equal(t, enabled, response.GetEnabled())
			require.Equal(t, enabled, manager.rule.Enabled)
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
	rackID := uuid.New()
	tests := map[string]struct {
		scope    *pb.EventRuleScope
		bindErr  error
		wantCode codes.Code
	}{
		"creates Site binding":        {},
		"legacy canonical Rack scope": {scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, Id: &pb.UUID{Id: rackID.String()}}},
		"external Rack scope":         {scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, RackExternalId: proto.String("rack-17")}},
		"invalid rack scope":          {bindErr: fmt.Errorf("%w: rack does not exist", eventrule.ErrInvalidRuleInput), wantCode: codes.InvalidArgument},
		"hierarchy conflict":          {bindErr: fmt.Errorf("%w: mixed scope", eventrule.ErrBindingConflict), wantCode: codes.FailedPrecondition},
	}
	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			manager := &mockEventRuleManager{bindErr: test.bindErr}
			inventory := newMockManager()
			inventory.racks[rackID] = &rack.Rack{Info: deviceinfo.DeviceInfo{ID: rackID}, ExternalID: "rack-17"}
			server := &FlowServerImpl{eventRuleManager: manager, inventoryManager: inventory}
			rule := manager.ensureRule()
			scope := test.scope
			wantScope := eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: rackID}
			if scope == nil {
				scope = &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE}
				wantScope = eventrule.Scope{Type: eventrule.ScopeTypeSite}
			}
			response, err := server.CreateEventRuleBinding(t.Context(), &pb.CreateEventRuleBindingRequest{RuleId: &pb.UUID{Id: rule.ID.String()}, Scope: scope})
			require.Equal(t, test.wantCode, status.Code(err))
			if test.wantCode != codes.OK {
				require.Nil(t, response)
				return
			}
			require.NotNil(t, response)
			require.Equal(t, wantScope, manager.binding.Scope)
			require.Nil(t, response.GetScope().RackExternalId)
			if wantScope.Type == eventrule.ScopeTypeRack {
				require.Equal(t, rackID.String(), response.GetScope().GetId().GetId())
			} else {
				require.Nil(t, response.GetScope().GetId())
			}
		})
	}
}

func TestFlowServerImpl_GetEventRuleBinding(t *testing.T) {
	rackID := uuid.New()
	legacyRequest := &pb.GetEventRuleBindingRequest{}
	require.NoError(t, protojson.Unmarshal([]byte(`{"eventType":"hardware.leak.detected","scope":{"type":"EVENT_RULE_SCOPE_TYPE_SITE"}}`), legacyRequest))
	for _, test := range []struct {
		name       string
		request    *pb.GetEventRuleBindingRequest
		found      bool
		wantLookup bool
		wantScope  eventrule.Scope
		wantCode   codes.Code
	}{
		{name: "legacy Site request with absent binding", request: legacyRequest, wantLookup: true, wantScope: eventrule.Scope{Type: eventrule.ScopeTypeSite}, wantCode: codes.NotFound},
		{name: "legacy canonical Rack scope", request: &pb.GetEventRuleBindingRequest{EventType: "hardware.leak.detected", Scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, Id: &pb.UUID{Id: rackID.String()}}}, found: true, wantLookup: true, wantScope: eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: rackID}},
		{name: "external Rack scope", request: &pb.GetEventRuleBindingRequest{EventType: "hardware.leak.detected", Scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, RackExternalId: proto.String("rack-17")}}, found: true, wantLookup: true, wantScope: eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: rackID}},
		{name: "unknown public Rack", request: &pb.GetEventRuleBindingRequest{EventType: "hardware.leak.detected", Scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, RackExternalId: proto.String("missing-rack")}}, wantCode: codes.NotFound},
		{name: "invalid canonical scope", request: &pb.GetEventRuleBindingRequest{EventType: "hardware.leak.detected", Scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK}}, wantCode: codes.InvalidArgument},
	} {
		t.Run(test.name, func(t *testing.T) {
			manager := &mockEventRuleManager{}
			if test.found {
				manager.binding = &eventrule.Binding{ID: uuid.New(), RuleID: uuid.New(), EventType: "hardware.leak.detected", Scope: test.wantScope}
			}
			inventory := newMockManager()
			inventory.racks[rackID] = &rack.Rack{Info: deviceinfo.DeviceInfo{ID: rackID}, ExternalID: "rack-17"}
			server := &FlowServerImpl{eventRuleManager: manager, inventoryManager: inventory}
			response, err := server.GetEventRuleBinding(t.Context(), test.request)
			require.Equal(t, test.wantCode, status.Code(err))
			if test.wantLookup {
				require.Equal(t, 1, manager.bindingLookupCalls)
				require.Equal(t, test.wantScope, manager.bindingLookupScope)
				require.Equal(t, eventrule.Type("hardware.leak.detected"), manager.bindingLookupEventType)
			} else {
				require.Zero(t, manager.bindingLookupCalls)
			}
			if test.wantCode != codes.OK {
				require.Nil(t, response)
				return
			}
			require.Equal(t, rackID.String(), response.GetScope().GetId().GetId())
			require.Nil(t, response.GetScope().RackExternalId)
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
			scope := &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE}
			if test.rackScope {
				scope.Type = pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK
				scope.RackExternalId = proto.String("rack-17")
			}
			response, err := server.PutEventRuleBinding(t.Context(), &pb.PutEventRuleBindingRequest{RuleId: &pb.UUID{Id: policyID.String()}, EventType: "hardware.leak.detected", Scope: scope})
			require.Equal(t, test.wantCode, status.Code(err))
			if test.wantCode != codes.OK {
				require.Nil(t, response)
				return
			}
			require.Equal(t, test.created, response.GetCreated())
			require.Nil(t, response.GetBinding().GetScope().RackExternalId)
			require.Equal(t, policyID.String(), response.GetBinding().GetRuleId().GetId())
			require.Equal(t, "hardware.leak.detected", response.GetBinding().GetEventType())
			if test.rackScope {
				require.Equal(t, rackID, manager.binding.Scope.ID)
				require.Equal(t, rackID.String(), response.GetBinding().GetScope().GetId().GetId())
				require.Equal(t, "rack-17", response.GetBinding().GetRackExternalId())
			} else {
				require.Equal(t, eventrule.Scope{Type: eventrule.ScopeTypeSite}, manager.binding.Scope)
			}
		})
	}
}

func TestFlowServerImpl_DeleteEventRuleBinding(t *testing.T) {
	policyID, rackID := uuid.New(), uuid.New()
	eventType := eventrule.Type("hardware.leak.detected")
	siteScope := eventrule.Scope{Type: eventrule.ScopeTypeSite}
	validRequest := func() *pb.DeleteEventRuleBindingRequest {
		return &pb.DeleteEventRuleBindingRequest{
			EventType:      string(eventType),
			ExpectedRuleId: &pb.UUID{Id: policyID.String()},
			Scope: &pb.EventRuleScope{
				Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE,
			},
		}
	}

	legacyRequest := &pb.DeleteEventRuleBindingRequest{}
	require.NoError(t, protojson.Unmarshal([]byte(`{"eventType":"hardware.leak.detected","scope":{"type":"EVENT_RULE_SCOPE_TYPE_SITE"}}`), legacyRequest))
	tests := map[string]struct {
		request    *pb.DeleteEventRuleBindingRequest
		unbindErr  error
		wantCode   codes.Code
		wantRuleID uuid.UUID
		wantScope  eventrule.Scope
	}{
		"deletes resolution slot": {
			request:    validRequest(),
			wantCode:   codes.OK,
			wantRuleID: policyID,
		},
		"legacy canonical Rack scope without expected policy": {
			request:   &pb.DeleteEventRuleBindingRequest{EventType: string(eventType), Scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, Id: &pb.UUID{Id: rackID.String()}}},
			wantScope: eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: rackID},
		},
		"external Rack scope with expected policy": {
			request:   &pb.DeleteEventRuleBindingRequest{EventType: string(eventType), Scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, RackExternalId: proto.String("rack-17")}, ExpectedRuleId: &pb.UUID{Id: policyID.String()}},
			wantScope: eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: rackID}, wantRuleID: policyID,
		},
		"invalid event type": {
			request: &pb.DeleteEventRuleBindingRequest{
				EventType:      "Invalid",
				Scope:          validRequest().Scope,
				ExpectedRuleId: validRequest().ExpectedRuleId,
			},
			wantCode: codes.InvalidArgument,
		},
		"legacy request without expected policy": {
			request:  legacyRequest,
			wantCode: codes.OK,
		},
		"empty expected policy": {
			request:  &pb.DeleteEventRuleBindingRequest{EventType: string(eventType), Scope: validRequest().Scope, ExpectedRuleId: &pb.UUID{}},
			wantCode: codes.InvalidArgument,
		},
		"zero expected policy": {
			request:  &pb.DeleteEventRuleBindingRequest{EventType: string(eventType), Scope: validRequest().Scope, ExpectedRuleId: &pb.UUID{Id: uuid.Nil.String()}},
			wantCode: codes.InvalidArgument,
		},
		"malformed expected policy": {
			request:  &pb.DeleteEventRuleBindingRequest{EventType: string(eventType), Scope: validRequest().Scope, ExpectedRuleId: &pb.UUID{Id: "invalid"}},
			wantCode: codes.InvalidArgument,
		},
		"policy replaced": {
			request:   validRequest(),
			unbindErr: eventrule.ErrBindingRuleMismatch,
			wantCode:  codes.AlreadyExists,
		},
		"missing scope": {
			request: &pb.DeleteEventRuleBindingRequest{
				EventType:      string(eventType),
				ExpectedRuleId: &pb.UUID{Id: policyID.String()},
			},
			wantCode: codes.InvalidArgument,
		},
		"unknown public Rack": {
			request: &pb.DeleteEventRuleBindingRequest{
				EventType:      string(eventType),
				ExpectedRuleId: &pb.UUID{Id: policyID.String()},
				Scope:          &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, RackExternalId: proto.String("missing-rack")},
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
			inventory := newMockManager()
			inventory.racks[rackID] = &rack.Rack{Info: deviceinfo.DeviceInfo{ID: rackID}, ExternalID: "rack-17"}
			server := &FlowServerImpl{eventRuleManager: manager, inventoryManager: inventory}

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
			wantScope := test.wantScope
			if wantScope.Type == "" {
				wantScope = siteScope
			}
			require.Equal(t, wantScope, manager.unboundScope)
			require.Equal(t, test.wantRuleID, manager.unboundRuleID)
		})
	}
}

func TestFlowServerImpl_resolveEventRuleScope(t *testing.T) {
	rackID, externalRackID := uuid.New(), uuid.New()
	uuidExternalID := uuid.NewString()
	inventory := newMockManager()
	inventory.racks[rackID] = &rack.Rack{Info: deviceinfo.DeviceInfo{ID: rackID}, ExternalID: "rack-external-1"}
	inventory.racks[externalRackID] = &rack.Rack{Info: deviceinfo.DeviceInfo{ID: externalRackID}, ExternalID: uuidExternalID}
	decoyID := uuid.MustParse(uuidExternalID)
	inventory.racks[decoyID] = &rack.Rack{Info: deviceinfo.DeviceInfo{ID: decoyID}, ExternalID: "different-external-id"}
	for _, test := range []struct {
		name        string
		scope       *pb.EventRuleScope
		noInventory bool
		wantScope   eventrule.Scope
		wantCode    codes.Code
	}{
		{name: "Site needs no inventory lookup", scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE}, noInventory: true, wantScope: eventrule.Scope{Type: eventrule.ScopeTypeSite}},
		{name: "canonical Rack UUID needs no inventory lookup", scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, Id: &pb.UUID{Id: rackID.String()}}, noInventory: true, wantScope: eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: rackID}},
		{name: "external Rack ID", scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, RackExternalId: proto.String("rack-external-1")}, wantScope: eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: rackID}},
		{name: "UUID-shaped external ID does not select an internal UUID", scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, RackExternalId: &uuidExternalID}, wantScope: eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: externalRackID}},
		{name: "missing scope", wantCode: codes.InvalidArgument},
		{name: "Site with canonical ID", scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE, Id: &pb.UUID{Id: rackID.String()}}, wantCode: codes.InvalidArgument},
		{name: "Site with external ID", scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE, RackExternalId: proto.String("rack-external-1")}, wantCode: codes.InvalidArgument},
		{name: "Rack without identifier", scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK}, wantCode: codes.InvalidArgument},
		{name: "both identifiers rejected before lookup", scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, Id: &pb.UUID{Id: rackID.String()}, RackExternalId: proto.String("rack-external-1")}, noInventory: true, wantCode: codes.InvalidArgument},
		{name: "empty external ID rejected before lookup", scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, RackExternalId: proto.String("")}, noInventory: true, wantCode: codes.InvalidArgument},
		{name: "malformed canonical Rack UUID", scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, Id: &pb.UUID{Id: "not-a-uuid"}}, wantCode: codes.InvalidArgument},
		{name: "zero canonical Rack UUID", scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, Id: &pb.UUID{Id: uuid.Nil.String()}}, wantCode: codes.InvalidArgument},
		{name: "unknown scope type", scope: &pb.EventRuleScope{}, wantCode: codes.InvalidArgument},
		{name: "unknown external Rack", scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, RackExternalId: proto.String("missing-rack")}, wantCode: codes.NotFound},
		{name: "external ID needs inventory", scope: &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK, RackExternalId: proto.String("rack-external-1")}, noInventory: true, wantCode: codes.FailedPrecondition},
	} {
		t.Run(test.name, func(t *testing.T) {
			server := &FlowServerImpl{}
			if !test.noInventory {
				server.inventoryManager = inventory
			}
			got, err := server.resolveEventRuleScope(t.Context(), test.scope)
			require.Equal(t, test.wantCode, status.Code(err))
			if test.wantCode != codes.OK {
				return
			}
			require.Equal(t, test.wantScope, got)
		})
	}
}

type mockEventRuleManager struct {
	bindingLookupCalls     int
	bindingLookupScope     eventrule.Scope
	bindingLookupEventType eventrule.Type
	bindingQuery           eventrule.BindingListRequest
	bindingCreated         bool
	query                  eventrule.HandlingQuery
	eventRuleManager
	rule             *eventrule.Rule
	createCalls      int
	createInput      eventrule.RuleCreate
	createErr        error
	getByIDCalls     int
	setEnabledCalls  int
	setEnabledID     uuid.UUID
	setEnabledErr    error
	laterEnabled     *bool
	bindErr          error
	unbindErr        error
	binding          *eventrule.Binding
	effectiveRule    *eventrule.Rule
	effectiveTarget  eventrule.ResourceIdentity
	listPage         eventrule.RuleListPage
	listRequest      eventrule.RuleListRequest
	listCalls        int
	unboundEventType eventrule.Type
	unboundRuleID    uuid.UUID
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
	m.createInput = input
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
	m.getByIDCalls++
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

func (m *mockEventRuleManager) SetEnabled(_ context.Context, id uuid.UUID, enabled bool) (*eventrule.Rule, error) {
	m.setEnabledCalls++
	m.setEnabledID = id
	if m.setEnabledErr != nil {
		return nil, m.setEnabledErr
	}
	m.ensureRule().Enabled = enabled
	snapshot := m.rule.Clone()
	if m.laterEnabled != nil {
		m.rule.Enabled = *m.laterEnabled
	}
	return &snapshot, nil
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
	m.binding = &eventrule.Binding{
		ID:        uuid.New(),
		RuleID:    ruleID,
		EventType: m.ensureRule().EventType,
		Scope:     scope,
	}
	return m.binding, nil
}

func (m *mockEventRuleManager) GetBindingForScope(
	_ context.Context,
	eventType eventrule.Type,
	scope eventrule.Scope,
) (*eventrule.Binding, error) {
	m.bindingLookupCalls++
	m.bindingLookupScope = scope
	m.bindingLookupEventType = eventType
	return m.binding, nil
}

func (m *mockEventRuleManager) Unbind(
	_ context.Context,
	eventType eventrule.Type,
	scope eventrule.Scope,
	expectedRuleID uuid.UUID,
) error {
	m.unboundEventType = eventType
	m.unboundScope = scope
	m.unboundRuleID = expectedRuleID

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
		{name: "wrapped database failure", managerErr: fmt.Errorf("list event handlings: %w", &pgconn.PgError{Code: "08006", Message: "database connection failed"}), code: codes.Internal},
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
			if tc.managerErr != nil {
				require.Contains(t, status.Convert(err).Message(), tc.managerErr.Error())
			}
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
