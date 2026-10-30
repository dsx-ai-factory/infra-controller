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
		name       string
		manager    *mockEventRuleManager
		mutate     func(*pb.CreateEventRuleRequest)
		wantCode   codes.Code
		wantCalled bool
	}{
		{name: "creates disabled rule", manager: &mockEventRuleManager{}, wantCalled: true},
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
			require.False(t, response.GetEnabled(), "new persisted rules are disabled")
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

func TestFlowServerImpl_UpdateEventRule(t *testing.T) {
	tests := []struct {
		name     string
		manager  *mockEventRuleManager
		request  func(uuid.UUID) *pb.UpdateEventRuleRequest
		wantCode codes.Code
	}{
		{
			name:    "metadata",
			manager: &mockEventRuleManager{},
			request: func(id uuid.UUID) *pb.UpdateEventRuleRequest {
				return &pb.UpdateEventRuleRequest{
					RuleId: &pb.UUID{Id: id.String()},
					Update: &pb.UpdateEventRuleRequest_Metadata{
						Metadata: &pb.EventRuleMetadataUpdate{Name: "updated"},
					},
				}
			},
		},
		{
			name: "immutable built-in",
			manager: &mockEventRuleManager{
				updateMetadataErr: eventrule.ErrBuiltInRuleImmutable,
			},
			request: func(id uuid.UUID) *pb.UpdateEventRuleRequest {
				return &pb.UpdateEventRuleRequest{
					RuleId: &pb.UUID{Id: id.String()},
					Update: &pb.UpdateEventRuleRequest_Metadata{
						Metadata: &pb.EventRuleMetadataUpdate{Name: "updated"},
					},
				}
			},
			wantCode: codes.FailedPrecondition,
		},
		{
			name:    "missing update",
			manager: &mockEventRuleManager{},
			request: func(id uuid.UUID) *pb.UpdateEventRuleRequest {
				return &pb.UpdateEventRuleRequest{RuleId: &pb.UUID{Id: id.String()}}
			},
			wantCode: codes.InvalidArgument,
		},
		{
			name: "store failure",
			manager: &mockEventRuleManager{
				updateMetadataErr: errors.New("store unavailable"),
			},
			request: func(id uuid.UUID) *pb.UpdateEventRuleRequest {
				return &pb.UpdateEventRuleRequest{
					RuleId: &pb.UUID{Id: id.String()},
					Update: &pb.UpdateEventRuleRequest_Metadata{
						Metadata: &pb.EventRuleMetadataUpdate{Name: "updated"},
					},
				}
			},
			wantCode: codes.Internal,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			server := &FlowServerImpl{eventRuleManager: test.manager}
			rule := test.manager.ensureRule()
			response, err := server.UpdateEventRule(
				context.Background(),
				test.request(rule.ID),
			)

			require.Equal(t, test.wantCode, status.Code(err))
			if test.wantCode == codes.OK {
				require.NotNil(t, response)
				return
			}
			require.Nil(t, response)
		})
	}
}

func TestFlowServerImpl_EnableEventRule(t *testing.T) {
	tests := []struct {
		name     string
		manager  *mockEventRuleManager
		wantCode codes.Code
	}{
		{name: "enables rule", manager: &mockEventRuleManager{}},
		{
			name: "rejects read-only rule",
			manager: &mockEventRuleManager{
				setEnabledErr: eventrule.ErrBuiltInRuleImmutable,
			},
			wantCode: codes.FailedPrecondition,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			server := &FlowServerImpl{eventRuleManager: test.manager}
			rule := test.manager.ensureRule()

			response, err := server.EnableEventRule(
				context.Background(),
				&pb.EnableEventRuleRequest{RuleId: &pb.UUID{Id: rule.ID.String()}},
			)

			require.Equal(t, test.wantCode, status.Code(err))
			if test.wantCode != codes.OK {
				require.Nil(t, response)
				return
			}
			require.True(t, response.GetEnabled())
		})
	}
}

func TestFlowServerImpl_DisableEventRule(t *testing.T) {
	manager := &mockEventRuleManager{rule: validAPIEventRule(eventrule.RuleOriginPersisted)}
	manager.rule.Enabled = true
	server := &FlowServerImpl{eventRuleManager: manager}

	response, err := server.DisableEventRule(
		context.Background(),
		&pb.DisableEventRuleRequest{RuleId: &pb.UUID{Id: manager.rule.ID.String()}},
	)

	require.NoError(t, err)
	require.False(t, response.GetEnabled())
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

func TestFlowServerImpl_GetEventRuleBindingReturnsNotFound(t *testing.T) {
	manager := &mockEventRuleManager{}
	server := &FlowServerImpl{eventRuleManager: manager}

	response, err := server.GetEventRuleBinding(
		context.Background(),
		&pb.GetEventRuleBindingRequest{
			EventType: "hardware.leak.detected",
			Scope:     &pb.EventRuleScope{Type: pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE},
		},
	)

	require.Nil(t, response)
	require.Equal(t, codes.NotFound, status.Code(err))
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
		"binding not found": {
			request:   validRequest(),
			unbindErr: fmt.Errorf("%w: missing binding", eventrule.ErrBindingNotFound),
			wantCode:  codes.NotFound,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			manager := &mockEventRuleManager{unbindErr: test.unbindErr}
			server := &FlowServerImpl{eventRuleManager: manager}

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

func TestFlowServerImpl_GetEventHandling(t *testing.T) {
	componentUUID, rackUUID, eventID := uuid.New(), uuid.New(), uuid.New()
	handledAt := time.Date(2026, 10, 26, 16, 0, 0, 0, time.UTC)
	tests := []struct {
		name          string
		resourceID    string
		resourceType  pb.EventResourceType
		limit         int32
		wantKind      eventrule.ResourceKind
		wantID        uuid.UUID
		wantLimit     int
		wantCode      codes.Code
		managerErr    error
		invalidPolicy bool
		offset        int32
		total         int32
	}{
		{name: "component snapshot and offset", resourceID: "component-17", resourceType: pb.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT, limit: 20, wantKind: eventrule.ResourceKindComponent, wantID: componentUUID, wantLimit: 20, offset: 5, total: 23},
		{name: "rack source", resourceID: "rack-17", resourceType: pb.EventResourceType_EVENT_RESOURCE_TYPE_RACK, wantKind: eventrule.ResourceKindRack, wantID: rackUUID, wantLimit: 1, total: 1},
		{name: "negative offset", resourceID: "component-17", resourceType: pb.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT, offset: -1, wantCode: codes.InvalidArgument},
		{name: "missing source", wantCode: codes.InvalidArgument},
		{name: "invalid manager input", resourceID: "component-17", resourceType: pb.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT, managerErr: fmt.Errorf("%w: unsupported event type", eventrule.ErrInvalidRuleInput), wantCode: codes.InvalidArgument},
		{name: "invalid stored action", resourceID: "component-17", resourceType: pb.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT, invalidPolicy: true, wantCode: codes.Internal},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			policy := eventrule.Policy{Actions: []eventrule.Action{{Name: "record", Spec: &eventrule.Noop{Reason: "record leakage"}}}}
			if test.invalidPolicy {
				policy.Actions[0].Spec = &eventrule.SubmitTask{}
			}
			ruleManager := &mockEventRuleManager{handlingErr: test.managerErr, handlingTotal: int(test.total), handling: []eventrule.Handling{{
				ID:            eventID,
				EventType:     eventrule.Type("hardware.leak.detected"),
				EventSummary:  "Leak detected",
				AppliedPolicy: policy,
				Executions:    []eventrule.HandlingExecution{{ID: eventID, ActionName: "record", Status: eventrule.ExecutionStatusDeferred, Reason: eventrule.ExecutionReasonAttemptInterrupted}},
				ResourceID:    test.resourceID,
				ResourceType:  test.wantKind,
				HandledAt:     handledAt,
			}}}
			inventoryManager := newMockManager()
			inventoryManager.components[componentUUID] = &component.Component{Info: deviceinfo.DeviceInfo{ID: componentUUID}, ComponentID: "component-17"}
			inventoryManager.racks[rackUUID] = &rack.Rack{Info: deviceinfo.DeviceInfo{ID: rackUUID}, ExternalID: "rack-17"}
			server := &FlowServerImpl{eventRuleManager: ruleManager, inventoryManager: inventoryManager}
			response, err := server.GetEventHandling(t.Context(), &pb.GetEventHandlingRequest{
				EventType: "hardware.leak.detected", ResourceType: test.resourceType, ResourceId: test.resourceID, Offset: test.offset, Limit: test.limit,
			})
			require.Equal(t, test.wantCode, status.Code(err))
			if test.wantCode != codes.OK {
				return
			}
			require.NoError(t, err)
			require.Equal(t, eventrule.Type("hardware.leak.detected"), ruleManager.handlingEventType)
			require.Equal(t, test.wantKind, ruleManager.handlingResourceType)
			require.Equal(t, test.wantID, ruleManager.handlingResourceID)
			require.Equal(t, test.wantLimit, ruleManager.handlingLimit)
			require.Equal(t, int(test.offset), ruleManager.handlingOffset)
			require.Len(t, response.GetHandlings(), 1)
			item := response.GetHandlings()[0]
			require.Equal(t, eventID.String(), item.GetId().GetId())
			require.Equal(t, "Leak detected", item.GetEventSummary())
			require.Equal(t, pb.EventHandlingStatus_EVENT_HANDLING_STATUS_RETRYING, item.GetHandlingStatus())
			require.Equal(t, pb.EventHandlingReason_EVENT_HANDLING_REASON_ATTEMPT_INTERRUPTED, item.GetReason())
			require.Equal(t, "A handling attempt was interrupted; another attempt is scheduled.", item.GetMessage())
			require.Equal(t, "record leakage", item.GetAppliedPolicy().GetActions()[0].GetNoop().GetReason())
			require.Equal(t, test.total, response.GetTotalCount())
		})
	}
}

func TestEventHandlingLimit(t *testing.T) {
	tests := map[string]struct {
		requested int32
		want      int
		wantError bool
	}{
		"default":       {want: 1},
		"minimum":       {requested: 1, want: 1},
		"maximum":       {requested: maxEventHandlingLimit, want: maxEventHandlingLimit},
		"negative":      {requested: -1, wantError: true},
		"above maximum": {requested: maxEventHandlingLimit + 1, wantError: true},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			got, err := eventHandlingLimit(test.requested)
			if test.wantError {
				require.EqualError(t, err, "limit must be between 1 and 100")
				return
			}

			require.NoError(t, err)
			require.Equal(t, test.want, got)
		})
	}
}

type mockEventRuleManager struct {
	scopedPolicies       []eventrule.ScopedPolicy
	scopedTotal          int
	scopedOffset         int
	scopedLimit          int
	scopedEventType      eventrule.Type
	rule                 *eventrule.Rule
	createCalls          int
	createErr            error
	updateMetadataErr    error
	setEnabledErr        error
	bindErr              error
	unbindErr            error
	binding              *eventrule.Binding
	effectiveRule        *eventrule.Rule
	effectiveTarget      eventrule.ResourceIdentity
	listPage             eventrule.RuleListPage
	listRequest          eventrule.RuleListRequest
	listCalls            int
	unboundEventType     eventrule.Type
	unboundScope         eventrule.Scope
	handling             []eventrule.Handling
	handlingTotal        int
	handlingErr          error
	handlingEventType    eventrule.Type
	handlingResourceType eventrule.ResourceKind
	handlingResourceID   uuid.UUID
	handlingLimit        int
	handlingOffset       int
}

func (m *mockEventRuleManager) ListScopedPolicies(_ context.Context, eventType eventrule.Type, offset, limit int) ([]eventrule.ScopedPolicy, int, error) {
	m.scopedEventType, m.scopedOffset, m.scopedLimit = eventType, offset, limit
	return m.scopedPolicies, m.scopedTotal, nil
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

func (m *mockEventRuleManager) UpdateMetadata(
	_ context.Context,
	_ uuid.UUID,
	metadata eventrule.RuleMetadata,
) error {
	if m.updateMetadataErr != nil {
		return m.updateMetadataErr
	}
	rule := m.ensureRule()
	rule.Name = metadata.Name
	rule.Description = metadata.Description
	return nil
}

func (m *mockEventRuleManager) ReplaceActions(
	_ context.Context,
	_ uuid.UUID,
	actions []eventrule.Action,
) error {
	m.ensureRule().Actions = eventrule.CloneActions(actions)
	return nil
}

func (m *mockEventRuleManager) SetEnabled(_ context.Context, _ uuid.UUID, enabled bool) error {
	if m.setEnabledErr != nil {
		return m.setEnabledErr
	}
	m.ensureRule().Enabled = enabled
	return nil
}

func (*mockEventRuleManager) Delete(context.Context, uuid.UUID) error { return nil }

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

func (m *mockEventRuleManager) SetOverride(
	ctx context.Context,
	input eventrule.RuleCreate,
	_ eventrule.Scope,
) (*eventrule.Rule, error) {
	return m.Create(ctx, input)
}

func (m *mockEventRuleManager) GetEffectiveForScope(
	ctx context.Context,
	eventType eventrule.Type,
	_ eventrule.Scope,
) (*eventrule.Rule, error) {
	rule, err := m.GetEffective(ctx, eventType, eventrule.ResourceIdentity{})
	return rule, err
}

func (*mockEventRuleManager) DeleteOverride(context.Context, eventrule.Type, eventrule.Scope) error {
	return nil
}

func (m *mockEventRuleManager) GetEventHandling(
	_ context.Context,
	eventType eventrule.Type,
	resourceType eventrule.ResourceKind,
	resourceID uuid.UUID,
	offset int,
	limit int,
) ([]eventrule.Handling, int, error) {
	m.handlingEventType = eventType
	m.handlingResourceType = resourceType
	m.handlingResourceID = resourceID
	m.handlingLimit = limit
	m.handlingOffset = offset
	return m.handling, m.handlingTotal, m.handlingErr
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

func TestFlowServerImpl_ListScopedEventRules(t *testing.T) {
	for _, test := range []struct {
		name      string
		request   *pb.ListScopedEventRulesRequest
		wantCode  codes.Code
		wantLimit int
	}{
		{name: "scope metadata and pagination", request: &pb.ListScopedEventRulesRequest{EventType: "hardware.leak.detected", Pagination: &pb.Pagination{Offset: 1, Limit: 2}}, wantLimit: 2},
		{name: "omitted pagination uses defaults", request: &pb.ListScopedEventRulesRequest{EventType: "hardware.leak.detected"}, wantLimit: dbquery.DefaultPaginationLimit},
		{name: "maximum page size accepted", request: &pb.ListScopedEventRulesRequest{EventType: "hardware.leak.detected", Pagination: &pb.Pagination{Limit: eventrule.MaxScopedPolicyPageSize}}, wantLimit: eventrule.MaxScopedPolicyPageSize},
		{name: "negative offset", request: &pb.ListScopedEventRulesRequest{EventType: "hardware.leak.detected", Pagination: &pb.Pagination{Offset: -1, Limit: 1}}, wantCode: codes.InvalidArgument},
		{name: "empty pagination rejects zero limit", request: &pb.ListScopedEventRulesRequest{EventType: "hardware.leak.detected", Pagination: &pb.Pagination{}}, wantCode: codes.InvalidArgument},
		{name: "negative limit", request: &pb.ListScopedEventRulesRequest{EventType: "hardware.leak.detected", Pagination: &pb.Pagination{Limit: -1}}, wantCode: codes.InvalidArgument},
		{name: "oversized limit", request: &pb.ListScopedEventRulesRequest{EventType: "hardware.leak.detected", Pagination: &pb.Pagination{Limit: eventrule.MaxScopedPolicyPageSize + 1}}, wantCode: codes.InvalidArgument},
		{name: "missing event type", request: &pb.ListScopedEventRulesRequest{}, wantCode: codes.InvalidArgument},
	} {
		t.Run(test.name, func(t *testing.T) {
			rackID := "rack-a"
			manager := &mockEventRuleManager{scopedTotal: math.MaxInt32 + 1, scopedPolicies: []eventrule.ScopedPolicy{
				{Rule: validAPIEventRule(eventrule.RuleOriginBuiltIn), Scope: eventrule.Scope{Type: eventrule.ScopeTypeSite}},
				{Rule: validAPIEventRule(eventrule.RuleOriginPersisted), Scope: eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: uuid.New()}, RackID: &rackID},
			}}
			server := &FlowServerImpl{eventRuleManager: manager}
			result, err := server.ListScopedEventRules(t.Context(), test.request)
			require.Equal(t, test.wantCode, status.Code(err))
			if test.wantCode != codes.OK {
				require.Nil(t, result)
				require.Empty(t, manager.scopedEventType)
				return
			}
			require.Equal(t, eventrule.Type("hardware.leak.detected"), manager.scopedEventType)
			require.EqualValues(t, test.request.GetPagination().GetOffset(), manager.scopedOffset)
			require.Equal(t, test.wantLimit, manager.scopedLimit)
			require.EqualValues(t, math.MaxInt32+1, result.GetTotal())
			require.Len(t, result.GetPolicies(), 2)
			require.True(t, result.GetPolicies()[0].GetRule().GetReadOnly())
			require.Equal(t, pb.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE, result.GetPolicies()[0].GetScopeType())
			require.Nil(t, result.GetPolicies()[0].RackId)
			require.Equal(t, rackID, result.GetPolicies()[1].GetRackId())
		})
	}
}
