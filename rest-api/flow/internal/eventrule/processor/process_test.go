// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package processor

import (
	"context"
	"sync/atomic"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	memorystore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/store/memory"
	eventtarget "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/target"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/location"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
)

func TestProcessorProcessPersistsAtomicPlan(t *testing.T) {
	rackID := uuid.New()
	store := memorystore.New()
	rule := processorRuntimeRule(
		noopAction("always"),
		conditionalNoopAction("critical", eventrule.SeverityCritical),
	)
	notifier := &countingNotifier{}
	processor := runtimeProcessor(
		t,
		processorInventoryWithRack(rackID),
		rule,
		store,
		defaultTargetResolver(rackID),
		notifier,
	)

	envelope := runtimeEnvelope(rackID)
	envelope.Severity = eventrule.SeverityInfo
	envelope.Payload = []byte(`{"secret":"must-not-be-persisted"}`)

	require.NoError(t, processor.Process(context.Background(), envelope))

	events, err := store.Events()
	require.NoError(t, err)
	require.Len(t, events, 1)
	require.Equal(t, envelope.Key, events[0].Key)
	require.Equal(t, eventrule.ResourceIdentity{Kind: eventrule.ResourceKindRack, ID: rackID}, events[0].Resource)
	require.Equal(t, rule.ID, events[0].AppliedRuleID)
	require.Len(t, events[0].EffectivePolicy.Actions, 1)
	require.False(t, events[0].CreatedAt.IsZero())
	require.NotContains(t, events[0].Summary, string(envelope.Payload))

	executions, err := store.Executions()
	require.NoError(t, err)
	require.Len(t, executions, 1)
	require.Equal(t, events[0].ID, executions[0].EventID)
	require.Equal(t, "always", executions[0].ActionName)
	require.IsType(t, &eventrule.NoopPlan{}, executions[0].Plan)
	require.Equal(t, eventrule.ExecutionStatusPending, executions[0].Status)
	require.Zero(t, executions[0].Attempts)
	require.EqualValues(t, 1, notifier.calls.Load())
}

func TestProcessorProcessDeduplicatesAtEventBoundary(t *testing.T) {
	t.Run("persisted duplicate records observation and stops", func(t *testing.T) {
		rackID := uuid.New()
		store := memorystore.New()
		var ruleCalls atomic.Int32
		notifier := &countingNotifier{}
		rules := ruleResolverFunc(func(context.Context, eventrule.Type, uuid.UUID) (*eventrule.Rule, error) {
			ruleCalls.Add(1)
			return processorRuntimeRule(noopAction("once")), nil
		})
		processor := runtimeProcessorWithRules(
			t,
			processorInventoryWithRack(rackID),
			rules,
			store,
			defaultTargetResolver(rackID),
			notifier,
		)

		envelope := runtimeEnvelope(rackID)

		require.NoError(t, processor.Process(context.Background(), envelope))
		require.NoError(t, processor.Process(context.Background(), envelope))

		require.EqualValues(t, 1, ruleCalls.Load())
		require.EqualValues(t, 1, notifier.calls.Load())

		events, err := store.Events()
		require.NoError(t, err)
		require.Equal(t, 2, events[0].Observations)
	})

	t.Run("concurrent planner loser stops after atomic commit", func(t *testing.T) {
		rackID := uuid.New()
		store := memorystore.New()
		enteredCommit := make(chan struct{})
		releaseCommit := make(chan struct{})
		planStore := &blockingEventPlanStore{
			Store:   store,
			entered: enteredCommit,
			release: releaseCommit,
		}
		notifier := &countingNotifier{}
		processor, err := New(Config{
			Inventory: processorInventoryWithRack(rackID),
			Rules: ruleResolverFunc(func(context.Context, eventrule.Type, uuid.UUID) (*eventrule.Rule, error) {
				return processorRuntimeRule(noopAction("once")), nil
			}),
			Store:    planStore,
			Targets:  targetRegistry(t, defaultTargetResolver(rackID)),
			Notifier: notifier,
		})
		require.NoError(t, err)

		envelope := runtimeEnvelope(rackID)
		firstResult := make(chan error, 1)

		go func() {
			firstResult <- processor.Process(context.Background(), envelope)
		}()

		select {
		case <-enteredCommit:
		case <-time.After(5 * time.Second):
			close(releaseCommit)
			t.Fatal("first processor did not reach the event-plan commit")
		}

		require.NoError(t, processor.Process(context.Background(), envelope))
		close(releaseCommit)

		select {
		case err := <-firstResult:
			require.NoError(t, err)
		case <-time.After(5 * time.Second):
			t.Fatal("first processor did not finish the event-plan commit")
		}

		events, err := store.Events()
		require.NoError(t, err)
		require.Len(t, events, 1)
		require.Equal(t, 2, events[0].Observations)

		storedExecutions, err := store.Executions()
		require.NoError(t, err)
		require.Len(t, storedExecutions, 1)
		require.Equal(t, eventrule.ExecutionStatusPending, storedExecutions[0].Status)
		require.EqualValues(t, 1, notifier.calls.Load())
	})
}

func TestProcessorProcessPersistsEmptyEffectivePolicy(t *testing.T) {
	rackID := uuid.New()
	store := memorystore.New()
	notifier := &countingNotifier{}
	processor := runtimeProcessor(
		t,
		processorInventoryWithRack(rackID),
		processorRuntimeRule(conditionalNoopAction("critical", eventrule.SeverityCritical)),
		store,
		defaultTargetResolver(rackID),
		notifier,
	)

	envelope := runtimeEnvelope(rackID)
	envelope.Severity = eventrule.SeverityInfo

	require.NoError(t, processor.Process(context.Background(), envelope))

	events, err := store.Events()
	require.NoError(t, err)
	require.Empty(t, events[0].EffectivePolicy.Actions)

	executions, err := store.Executions()
	require.NoError(t, err)
	require.Empty(t, executions)
	require.EqualValues(t, 1, notifier.calls.Load())
}

func TestProcessorPlansConcreteSubmitTaskTargets(t *testing.T) {
	rackID := uuid.New()
	computeID := uuid.New()
	nvSwitchID := uuid.New()
	resolvedRack := rack.New(deviceinfo.DeviceInfo{ID: rackID}, location.Location{})
	resolvedRack.Components = []component.Component{
		component.New(devicetypes.ComponentTypeNVSwitch, &deviceinfo.DeviceInfo{ID: nvSwitchID}, "", nil),
		component.New(devicetypes.ComponentTypeCompute, &deviceinfo.DeviceInfo{ID: computeID}, "", nil),
	}

	store := memorystore.New()
	processor := runtimeProcessor(
		t,
		&processorInventory{rack: resolvedRack},
		processorRuntimeRule(submitAction("power_off")),
		store,
		defaultTargetResolver(rackID),
		nil,
	)

	require.NoError(t, processor.Process(context.Background(), runtimeEnvelope(rackID)))

	executions, err := store.Executions()
	require.NoError(t, err)

	plan := executions[0].Plan.(*eventrule.SubmitTaskPlan)
	require.Equal(t, operations.PowerOperationForcePowerOff.CodeString(), plan.Operation.Code)
	require.Equal(t, "ForcePowerOff, forced false", plan.Description)
	require.Len(t, plan.Targets, 1)
	require.Equal(t, rackID, plan.Targets[0].RackID)
	require.Equal(t, []uuid.UUID{computeID}, plan.Targets[0].ComponentsByType[devicetypes.ComponentTypeCompute])
	require.Equal(t, []uuid.UUID{nvSwitchID}, plan.Targets[0].ComponentsByType[devicetypes.ComponentTypeNVSwitch])
}

func TestProcessorPersistsNoTargetExecutionAsSkipped(t *testing.T) {
	rackID := uuid.New()
	store := memorystore.New()
	processor := runtimeProcessor(
		t,
		processorInventoryWithRack(rackID),
		processorRuntimeRule(submitAction("power_off")),
		store,
		&testTargetResolver{},
		nil,
	)

	require.NoError(t, processor.Process(context.Background(), runtimeEnvelope(rackID)))

	executions, err := store.Executions()
	require.NoError(t, err)
	require.Equal(t, eventrule.ExecutionStatusSkipped, executions[0].Status)
	require.Equal(t, eventrule.ExecutionReasonNoTargets, executions[0].Reason)
}

func runtimeProcessor(
	t *testing.T,
	inventory *processorInventory,
	rule *eventrule.Rule,
	store *memorystore.Store,
	targets eventtarget.Resolver,
	notifier ExecutionNotifier,
) *Processor {
	t.Helper()

	return runtimeProcessorWithRules(t, inventory, ruleResolverFunc(func(
		context.Context,
		eventrule.Type,
		uuid.UUID,
	) (*eventrule.Rule, error) {
		return rule, nil
	}), store, targets, notifier)
}

func runtimeProcessorWithRules(
	t *testing.T,
	inventory *processorInventory,
	rules RuleResolver,
	store *memorystore.Store,
	targets eventtarget.Resolver,
	notifier ExecutionNotifier,
) *Processor {
	t.Helper()

	processor, err := New(Config{
		Inventory: inventory,
		Rules:     rules,
		Store:     store,
		Targets:   targetRegistry(t, targets),
		Notifier:  notifier,
	})
	require.NoError(t, err)

	return processor
}

func newTestProcessor(t *testing.T, inventory *processorInventory, rules RuleResolver) *Processor {
	t.Helper()

	if rules == nil {
		rules = ruleResolverFunc(func(context.Context, eventrule.Type, uuid.UUID) (*eventrule.Rule, error) {
			return nil, nil
		})
	}

	store := memorystore.New()

	return runtimeProcessorWithRules(
		t,
		inventory,
		rules,
		store,
		defaultTargetResolver(uuid.New()),
		nil,
	)
}

func processorRuntimeRule(actions ...eventrule.Action) *eventrule.Rule {
	return &eventrule.Rule{ID: uuid.New(), EventType: "test.event", Policy: eventrule.Policy{Actions: actions}}
}

func runtimeEnvelope(rackID uuid.UUID) eventrule.Envelope {
	return eventrule.Envelope{
		Key:      eventrule.EventKey{SourceName: "test", SourceKey: uuid.NewString()},
		Type:     "test.event",
		Resource: eventrule.Resource{Kind: eventrule.ResourceKindRack, ID: rackID},
	}
}

func noopAction(name string) eventrule.Action {
	return eventrule.Action{Name: name, Spec: &eventrule.Noop{}}
}

func conditionalNoopAction(name string, severity eventrule.Severity) eventrule.Action {
	return eventrule.Action{
		Name:      name,
		Condition: eventrule.ActionCondition{Severities: []eventrule.Severity{severity}},
		Spec:      &eventrule.Noop{},
	}
}

func submitAction(name string) eventrule.Action {
	return eventrule.Action{Name: name, Spec: &eventrule.SubmitTask{
		Operation:        &operations.PowerControlTaskInfo{Operation: operations.PowerOperationForcePowerOff},
		TargetStrategy:   eventrule.TargetStrategyRack,
		ConflictStrategy: eventrule.ConflictStrategyQueue,
	}}
}

func targetRegistry(t *testing.T, resolver eventtarget.Resolver) *eventtarget.Registry {
	t.Helper()

	registry := eventtarget.New()
	require.NoError(t, registry.Register("test.event", eventrule.TargetStrategyRack, resolver))

	return registry
}

func defaultTargetResolver(rackID uuid.UUID) eventtarget.Resolver {
	return &testTargetResolver{targets: []eventtarget.Target{{
		Kind:   eventrule.ResourceKindRack,
		ID:     rackID,
		RackID: rackID,
	}}}
}

type testTargetResolver struct {
	targets []eventtarget.Target
	err     error
}

func (r *testTargetResolver) Resolve(context.Context, eventtarget.ResolveRequest) ([]eventtarget.Target, error) {
	return r.targets, r.err
}

type blockingEventPlanStore struct {
	*memorystore.Store
	blocked atomic.Bool
	entered chan struct{}
	release chan struct{}
}

func (s *blockingEventPlanStore) CommitEventPlan(
	ctx context.Context,
	event eventrule.Event,
	planned []eventrule.PlannedExecution,
) (*eventrule.Event, error) {
	if s.blocked.CompareAndSwap(false, true) {
		close(s.entered)

		select {
		case <-s.release:
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}

	return s.Store.CommitEventPlan(ctx, event, planned)
}

type countingNotifier struct {
	calls atomic.Int32
}

func (n *countingNotifier) Notify() {
	n.calls.Add(1)
}

func validProcessorConfig(t *testing.T) Config {
	t.Helper()

	store := memorystore.New()

	return Config{
		Inventory: &processorInventory{},
		Rules: ruleResolverFunc(func(context.Context, eventrule.Type, uuid.UUID) (*eventrule.Rule, error) {
			return nil, nil
		}),
		Store:   store,
		Targets: targetRegistry(t, defaultTargetResolver(uuid.New())),
	}
}

func processorInventoryWithRack(rackID uuid.UUID) *processorInventory {
	return &processorInventory{rack: rack.New(deviceinfo.DeviceInfo{ID: rackID}, location.Location{})}
}
