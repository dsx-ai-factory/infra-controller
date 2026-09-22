// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"fmt"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	eventexecutor "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/executor"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/leakage"
	eventscheduler "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/scheduler"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/store/memory"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func TestNewArrangesBuiltInRules(t *testing.T) {
	manager, err := New(testManagerConfig())
	require.NoError(t, err)

	expected := leakage.DefaultRule()
	actual, err := manager.GetByID(context.Background(), expected.ID)
	require.NoError(t, err)
	require.Equal(t, expected, actual)
}

func TestManagerUnifiedReadsAndMutationRouting(t *testing.T) {
	ctx := context.Background()
	rackID := uuid.New()
	config := testManagerConfig()
	config.Inventory = testInventory{
		rack: &rack.Rack{Info: deviceinfo.DeviceInfo{ID: rackID}},
	}
	manager, err := New(config)
	require.NoError(t, err)

	input := testRuleCreate(leakage.TypeHardwareLeakDetected, "new rule")
	created, err := manager.Create(ctx, input)
	require.NoError(t, err)
	require.NotEqual(t, uuid.Nil, created.ID)
	require.Equal(t, eventrule.RuleOriginPersisted, created.Origin)
	require.False(t, created.Enabled)

	input.Policy.Actions[0].Name = "changed"
	require.Equal(t, "noop", created.Actions[0].Name)

	builtIn := leakage.DefaultRule()
	for _, id := range []uuid.UUID{created.ID, builtIn.ID} {
		rule, err := manager.GetByID(ctx, id)
		require.NoError(t, err)
		require.Equal(t, id, rule.ID)
	}

	rules, err := manager.List(ctx, eventrule.RuleListRequest{Limit: 100})
	require.NoError(t, err)
	require.Equal(t, 2, rules.Total)
	require.Len(t, rules.Rules, 2)
	require.Equal(t, eventrule.RuleOriginPersisted, rules.Rules[0].Origin)
	require.Equal(t, eventrule.RuleOriginBuiltIn, rules.Rules[1].Origin)

	secondPage, err := manager.List(ctx, eventrule.RuleListRequest{Offset: 1, Limit: 1})
	require.NoError(t, err)
	require.Equal(t, 2, secondPage.Total)
	require.Len(t, secondPage.Rules, 1)
	require.Equal(t, eventrule.RuleOriginBuiltIn, secondPage.Rules[0].Origin)

	_, err = manager.Create(ctx, eventrule.RuleCreate{})
	require.Error(t, err)
	_, err = manager.Create(ctx, testRuleCreate("unsupported.event", "unsupported"))
	require.ErrorIs(t, err, eventrule.ErrInvalidRuleInput)
	require.ErrorContains(t, err, `unsupported event type "unsupported.event"`)

	unsupportedEventType := eventrule.Type("unsupported.event")
	_, err = manager.List(ctx, eventrule.RuleListRequest{
		Filter: eventrule.RuleFilter{EventType: &unsupportedEventType},
		Limit:  100,
	})
	require.ErrorIs(t, err, eventrule.ErrInvalidRuleInput)

	require.Error(t, manager.UpdateMetadata(
		ctx,
		created.ID,
		eventrule.RuleMetadata{},
	))
	require.Error(t, manager.ReplaceActions(ctx, created.ID, nil))

	metadata := eventrule.RuleMetadata{
		Name:        "updated",
		Description: "updated description",
	}
	require.NoError(t, manager.UpdateMetadata(ctx, created.ID, metadata))

	updated, err := manager.GetByID(ctx, created.ID)
	require.NoError(t, err)
	require.Equal(t, metadata.Name, updated.Name)
	require.Equal(t, metadata.Description, updated.Description)

	actions := []eventrule.Action{
		{Name: "replacement", Spec: &eventrule.Noop{}},
	}
	require.NoError(t, manager.ReplaceActions(ctx, created.ID, actions))

	actions[0].Name = "changed"
	updated, err = manager.GetByID(ctx, created.ID)
	require.NoError(t, err)
	require.Equal(t, "replacement", updated.Actions[0].Name)

	require.NoError(t, manager.SetEnabled(ctx, created.ID, true))
	updated, err = manager.GetByID(ctx, created.ID)
	require.NoError(t, err)
	require.True(t, updated.Enabled)

	scope := eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: rackID}
	binding, err := manager.Bind(ctx, created.ID, scope)
	require.NoError(t, err)
	require.NotEqual(t, uuid.Nil, binding.ID)
	require.Equal(t, created.ID, binding.RuleID)
	require.Equal(t, created.EventType, binding.EventType)
	require.Equal(t, scope, binding.Scope)
}

func TestManager_SetEnabled(t *testing.T) {
	now := time.Date(2026, 8, 31, 12, 0, 0, 0, time.UTC)
	config := testManagerConfig()
	config.Store = memory.NewWithClock(func() time.Time { return now })
	manager, err := New(config)
	require.NoError(t, err)
	rule, err := manager.Create(
		context.Background(),
		testRuleCreate(leakage.TypeHardwareLeakDetected, "test rule"),
	)
	require.NoError(t, err)

	now = now.Add(time.Minute)
	require.NoError(t, manager.SetEnabled(context.Background(), rule.ID, true))
	enabled, err := manager.GetByID(context.Background(), rule.ID)
	require.NoError(t, err)
	require.True(t, enabled.Enabled)
	require.Equal(t, now, enabled.UpdatedAt)

	now = now.Add(time.Minute)
	require.NoError(t, manager.SetEnabled(context.Background(), rule.ID, true))
	unchanged, err := manager.GetByID(context.Background(), rule.ID)
	require.NoError(t, err)
	require.Equal(t, enabled.UpdatedAt, unchanged.UpdatedAt)

	require.NoError(t, manager.SetEnabled(context.Background(), rule.ID, false))
	disabled, err := manager.GetByID(context.Background(), rule.ID)
	require.NoError(t, err)
	require.False(t, disabled.Enabled)
	require.Equal(t, now, disabled.UpdatedAt)

	now = now.Add(time.Minute)
	require.NoError(t, manager.SetEnabled(context.Background(), rule.ID, false))
	unchanged, err = manager.GetByID(context.Background(), rule.ID)
	require.NoError(t, err)
	require.Equal(t, disabled.UpdatedAt, unchanged.UpdatedAt)
}

func TestManager_Bind(t *testing.T) {
	ctx := context.Background()
	manager, err := New(testManagerConfig())
	require.NoError(t, err)

	rule, err := manager.Create(
		ctx,
		testRuleCreate(leakage.TypeHardwareLeakDetected, "rack binding"),
	)
	require.NoError(t, err)

	missingRackID := uuid.New()
	binding, err := manager.Bind(ctx, rule.ID, eventrule.Scope{
		Type: eventrule.ScopeTypeRack,
		ID:   missingRackID,
	})

	require.Nil(t, binding)
	require.ErrorIs(t, err, eventrule.ErrInvalidRuleInput)
	require.ErrorContains(
		t,
		err,
		fmt.Sprintf("rack scope id %s does not identify an existing rack", missingRackID),
	)
}

func TestManager_GetEffective(t *testing.T) {
	rackID := uuid.New()
	componentID := uuid.New()
	config := testManagerConfig()
	config.Inventory = testInventory{
		rack: &rack.Rack{Info: deviceinfo.DeviceInfo{ID: rackID}},
		component: &component.Component{
			Info:   deviceinfo.DeviceInfo{ID: componentID},
			Type:   devicetypes.ComponentTypeCompute,
			RackID: rackID,
		},
	}
	manager, err := New(config)
	require.NoError(t, err)

	tests := []struct {
		name   string
		target eventrule.ResourceIdentity
	}{
		{
			name: "rack target",
			target: eventrule.ResourceIdentity{
				Kind: eventrule.ResourceKindRack,
				ID:   rackID,
			},
		},
		{
			name: "component target",
			target: eventrule.ResourceIdentity{
				Kind: eventrule.ResourceKindComponent,
				ID:   componentID,
			},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			rule, err := manager.GetEffective(
				context.Background(),
				leakage.TypeHardwareLeakDetected,
				test.target,
			)

			require.NoError(t, err)
			require.Equal(t, leakage.DefaultRule().ID, rule.ID)
		})
	}

	_, err = manager.GetEffective(
		context.Background(),
		leakage.TypeHardwareLeakDetected,
		eventrule.ResourceIdentity{Kind: eventrule.ResourceKindRack, ID: uuid.New()},
	)
	require.ErrorIs(t, err, eventrule.ErrRuleTargetNotFound)

	_, err = manager.GetEffective(
		context.Background(),
		"unsupported.event",
		eventrule.ResourceIdentity{Kind: eventrule.ResourceKindRack, ID: rackID},
	)
	require.ErrorIs(t, err, eventrule.ErrInvalidRuleInput)
	require.ErrorContains(t, err, `unsupported event type "unsupported.event"`)
}

func TestRuleResolver_GetEffective(t *testing.T) {
	ctx := context.Background()
	eventType := leakage.TypeHardwareLeakDetected
	rackID := uuid.New()
	config := testManagerConfig()
	config.Inventory = testInventory{
		rack: &rack.Rack{Info: deviceinfo.DeviceInfo{ID: rackID}},
	}
	manager, err := New(config)
	require.NoError(t, err)
	resolver := &ruleResolver{builtIns: manager.builtIns, store: manager.store}

	builtIn := leakage.DefaultRule()
	rule, err := resolver.GetEffective(ctx, eventType, rackID)
	require.NoError(t, err)
	require.Equal(t, builtIn.ID, rule.ID)

	site, err := manager.Create(ctx, testRuleCreate(eventType, "site"))
	require.NoError(t, err)
	_, err = manager.Bind(ctx, site.ID, eventrule.Scope{Type: eventrule.ScopeTypeSite})
	require.NoError(t, err)
	require.NoError(t, manager.SetEnabled(ctx, site.ID, true))

	rule, err = resolver.GetEffective(ctx, eventType, rackID)
	require.NoError(t, err)
	require.Equal(t, site.ID, rule.ID)

	rackRule, err := manager.Create(ctx, testRuleCreate(eventType, "rack"))
	require.NoError(t, err)
	_, err = manager.Bind(ctx, rackRule.ID, eventrule.Scope{
		Type: eventrule.ScopeTypeRack,
		ID:   rackID,
	})
	require.NoError(t, err)
	require.NoError(t, manager.SetEnabled(ctx, rackRule.ID, true))

	rule, err = resolver.GetEffective(ctx, eventType, rackID)
	require.NoError(t, err)
	require.Equal(t, rackRule.ID, rule.ID)

	require.NoError(t, manager.SetEnabled(ctx, rackRule.ID, false))
	rule, err = resolver.GetEffective(ctx, eventType, rackID)
	require.NoError(t, err)
	require.Equal(t, site.ID, rule.ID)

	require.NoError(t, manager.SetEnabled(ctx, site.ID, false))
	rule, err = resolver.GetEffective(ctx, eventType, rackID)
	require.NoError(t, err)
	require.Equal(t, builtIn.ID, rule.ID)

	rule, err = resolver.GetEffective(ctx, "unknown.event", rackID)
	require.NoError(t, err)
	require.Nil(t, rule)
}

func TestManagerRejectsMissingIDs(t *testing.T) {
	manager, err := New(testManagerConfig())
	require.NoError(t, err)

	_, err = manager.GetByID(context.Background(), uuid.Nil)
	require.ErrorContains(t, err, "event rule id is required")
	require.ErrorContains(t, manager.UpdateMetadata(
		context.Background(),
		uuid.Nil,
		eventrule.RuleMetadata{Name: "test"},
	), "event rule id is required")
	require.ErrorContains(t, manager.ReplaceActions(
		context.Background(),
		uuid.Nil,
		[]eventrule.Action{
			{Name: "noop", Spec: &eventrule.Noop{}},
		},
	), "event rule id is required")
	require.ErrorContains(
		t,
		manager.Delete(context.Background(), uuid.Nil),
		"event rule id is required",
	)
	require.ErrorContains(
		t,
		manager.SetEnabled(context.Background(), uuid.Nil, true),
		"event rule id is required",
	)

	_, err = manager.Bind(
		context.Background(),
		uuid.Nil,
		eventrule.Scope{Type: eventrule.ScopeTypeSite},
	)
	require.ErrorContains(t, err, "event rule id is required")
}

func TestManager_Unbind(t *testing.T) {
	manager, err := New(testManagerConfig())
	require.NoError(t, err)

	tests := map[string]struct {
		eventType eventrule.Type
		scope     eventrule.Scope
		wantIs    error
	}{
		"invalid event type": {
			eventType: "Invalid",
			scope:     eventrule.Scope{Type: eventrule.ScopeTypeSite},
			wantIs:    eventrule.ErrInvalidRuleInput,
		},
		"invalid scope": {
			eventType: leakage.TypeHardwareLeakDetected,
			scope:     eventrule.Scope{},
			wantIs:    eventrule.ErrInvalidRuleInput,
		},
		"unsupported event type": {
			eventType: "unsupported.event",
			scope:     eventrule.Scope{Type: eventrule.ScopeTypeSite},
			wantIs:    eventrule.ErrInvalidRuleInput,
		},
		"missing binding": {
			eventType: leakage.TypeHardwareLeakDetected,
			scope:     eventrule.Scope{Type: eventrule.ScopeTypeSite},
			wantIs:    eventrule.ErrBindingNotFound,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			err := manager.Unbind(context.Background(), test.eventType, test.scope)
			require.ErrorIs(t, err, test.wantIs)
		})
	}
}

func TestManager_GetBindingForScope(t *testing.T) {
	manager, err := New(testManagerConfig())
	require.NoError(t, err)

	tests := map[string]struct {
		eventType eventrule.Type
		scope     eventrule.Scope
	}{
		"invalid event type": {
			eventType: "Invalid",
			scope:     eventrule.Scope{Type: eventrule.ScopeTypeSite},
		},
		"invalid scope": {
			eventType: leakage.TypeHardwareLeakDetected,
			scope:     eventrule.Scope{},
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			binding, err := manager.GetBindingForScope(
				context.Background(),
				test.eventType,
				test.scope,
			)

			require.Nil(t, binding)
			require.ErrorIs(t, err, eventrule.ErrInvalidRuleInput)
		})
	}
}

func TestManagerRejectsBuiltInRuleMutations(t *testing.T) {
	builtIn := leakage.DefaultRule()
	manager, err := New(testManagerConfig())
	require.NoError(t, err)

	tests := map[string]func(context.Context) error{
		"update metadata": func(ctx context.Context) error {
			return manager.UpdateMetadata(
				ctx,
				builtIn.ID,
				eventrule.RuleMetadata{Name: "updated"},
			)
		},
		"replace actions": func(ctx context.Context) error {
			return manager.ReplaceActions(
				ctx,
				builtIn.ID,
				[]eventrule.Action{
					{Name: "noop", Spec: &eventrule.Noop{}},
				},
			)
		},
		"delete": func(ctx context.Context) error {
			return manager.Delete(ctx, builtIn.ID)
		},
		"bind": func(ctx context.Context) error {
			_, err := manager.Bind(
				ctx,
				builtIn.ID,
				eventrule.Scope{Type: eventrule.ScopeTypeSite},
			)

			return err
		},
		"set enabled": func(ctx context.Context) error {
			return manager.SetEnabled(ctx, builtIn.ID, false)
		},
	}

	for name, mutate := range tests {
		t.Run(name, func(t *testing.T) {
			err := mutate(context.Background())
			require.ErrorContains(t, err, "is a built-in and cannot be mutated")
		})
	}
}

func TestManagerValidatesConfiguredActionExecutors(t *testing.T) {
	tests := map[string]struct {
		alertSender *testAlertSender
		wantErr     string
	}{
		"rejects alert without sender": {
			wantErr: "no executor registered for action type \"send_alert\"",
		},
		"accepts alert with sender": {
			alertSender: &testAlertSender{},
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			config := testManagerConfig()
			if test.alertSender != nil {
				config.AlertSender = test.alertSender
			}

			manager, err := New(config)
			require.NoError(t, err)

			_, err = manager.Create(context.Background(), eventrule.RuleCreate{
				Metadata:  eventrule.RuleMetadata{Name: "alert"},
				EventType: leakage.TypeHardwareLeakDetected,
				Policy: eventrule.Policy{Actions: []eventrule.Action{
					{
						Name: "send",
						Spec: &eventrule.SendAlert{
							Severity: eventrule.SeverityWarning,
							Message:  "test alert",
						},
					},
				}},
			})
			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
				require.ErrorIs(t, err, eventrule.ErrInvalidRuleInput)
				return
			}

			require.NoError(t, err)
		})
	}
}

func testManagerConfig() Config {
	return Config{
		Store: memory.New(),
		Scheduler: SchedulerConfig{
			InstanceID: "event-rule-manager-test",
			Runtime:    eventscheduler.DefaultRuntimeConfig(),
			Policy:     eventscheduler.DefaultPolicyConfig(),
		},
		Inventory:   testInventory{},
		TaskManager: configTaskManager{},
	}
}

func testRuleCreate(
	eventType eventrule.Type,
	name string,
) eventrule.RuleCreate {
	return eventrule.RuleCreate{
		Metadata:  eventrule.RuleMetadata{Name: name},
		EventType: eventType,
		Policy: eventrule.Policy{Actions: []eventrule.Action{
			{Name: "noop", Spec: &eventrule.Noop{}},
		}},
	}
}

type testInventory struct {
	component *component.Component
	rack      *rack.Rack
}

func (i testInventory) GetComponentByID(
	_ context.Context,
	id uuid.UUID,
) (*component.Component, error) {
	if i.component == nil || i.component.Info.ID != id {
		return nil, nil
	}

	return i.component, nil
}

func (testInventory) GetComponentsByExternalIDs(
	context.Context,
	[]string,
) ([]*component.Component, error) {
	return nil, nil
}

func (i testInventory) GetRackByIdentifier(
	_ context.Context,
	ref identifier.Identifier,
	_ bool,
) (*rack.Rack, error) {
	if i.rack == nil || i.rack.Info.ID != ref.ID {
		return nil, nil
	}

	return i.rack, nil
}

type testAlertSender struct{}

func (*testAlertSender) SendAlert(
	context.Context,
	eventexecutor.AlertRequest,
) (string, error) {
	return uuid.NewString(), nil
}
