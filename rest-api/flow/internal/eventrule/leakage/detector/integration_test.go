// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package detector_test

import (
	"context"
	"fmt"
	"slices"
	"sync"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/ingestion"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/leakage"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/leakage/detector"
	eventrulemanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/manager"
	eventrulescheduler "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/scheduler"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/store/memory"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func TestDetector_EventRuleIntegration(t *testing.T) {
	tests := map[string]struct {
		strategy eventrule.TargetStrategy
		scope    eventrule.Scope
		wantIDs  func(sourceID, belowID, aboveID uuid.UUID) []uuid.UUID
	}{
		"immutable affected-components default": {
			strategy: eventrule.TargetStrategyAffectedComponents,
			wantIDs: func(sourceID, belowID, _ uuid.UUID) []uuid.UUID {
				return []uuid.UUID{sourceID, belowID}
			},
		},
		"site component override": {
			strategy: eventrule.TargetStrategyComponent,
			scope:    eventrule.Scope{Type: eventrule.ScopeTypeSite},
			wantIDs: func(sourceID, _, _ uuid.UUID) []uuid.UUID {
				return []uuid.UUID{sourceID}
			},
		},
		"rack override": {
			strategy: eventrule.TargetStrategyRack,
			scope:    eventrule.Scope{Type: eventrule.ScopeTypeRack},
			wantIDs: func(sourceID, belowID, aboveID uuid.UUID) []uuid.UUID {
				return []uuid.UUID{sourceID, belowID, aboveID}
			},
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			ctx := context.Background()
			rackID := uuid.New()
			sourceID := uuid.New()
			belowID := uuid.New()
			aboveID := uuid.New()

			inventory := newLeakageInventory(rackID, []component.Component{
				leakageComponent(sourceID, rackID, "machine-1", 10),
				leakageComponent(belowID, rackID, "machine-below", 1),
				leakageComponent(aboveID, rackID, "machine-above", 20),
			})

			tasks := &recordingTaskManager{}

			ruleManager, err := eventrulemanager.New(eventrulemanager.Config{
				Store: memory.New(),
				Scheduler: eventrulemanager.SchedulerConfig{
					InstanceID: "leakage-detector-integration",
					Runtime:    eventrulescheduler.DefaultRuntimeConfig(),
					Policy:     eventrulescheduler.DefaultPolicyConfig(),
				},
				Inventory:   inventory,
				TaskManager: tasks,
			})
			require.NoError(t, err)

			if test.scope.Type != "" {
				rule, err := ruleManager.Create(ctx, leakageRule(test.strategy))
				require.NoError(t, err)

				scope := test.scope
				if scope.Type == eventrule.ScopeTypeRack {
					scope.ID = rackID
				}

				_, err = ruleManager.Bind(ctx, rule.ID, scope)
				require.NoError(t, err)

				require.NoError(t, ruleManager.SetEnabled(ctx, rule.ID, true))
			}

			require.NoError(t, ruleManager.Start(ctx))
			defer func() { require.NoError(t, ruleManager.Stop()) }()

			leakDetector, err := detector.New(staticLeakReader{machines: []string{"machine-1"}})
			require.NoError(t, err)

			ingestionConfig := ingestion.DefaultConfig()
			ingestionConfig.RetryDelay = time.Millisecond
			pipeline, err := ingestion.New(ruleManager, ingestionConfig)
			require.NoError(t, err)

			source, err := pipeline.RegisterSource(detector.SourceName)
			require.NoError(t, err)

			require.NoError(t, leakDetector.Collect(ctx, source.Ingest))

			require.Eventually(t, func() bool {
				return tasks.count() == 1
			}, time.Second, time.Millisecond)

			wantIDs := test.wantIDs(sourceID, belowID, aboveID)
			slices.SortFunc(wantIDs, compareUUID)
			require.Equal(t, wantIDs, tasks.componentIDs())

			// The detector retains the occurrence key while the source stays active,
			// so repeated polling observes the event without creating another
			// execution or task.
			require.NoError(t, leakDetector.Collect(ctx, source.Ingest))
			require.Never(t, func() bool {
				return tasks.count() > 1
			}, 100*time.Millisecond, time.Millisecond)
		})
	}
}

func leakageRule(strategy eventrule.TargetStrategy) eventrule.RuleCreate {
	return eventrule.RuleCreate{
		Metadata:  eventrule.RuleMetadata{Name: "Leakage integration override"},
		EventType: leakage.TypeHardwareLeakDetected,
		Policy: eventrule.Policy{Actions: []eventrule.Action{{
			Name: "power_off",
			Spec: &eventrule.SubmitTask{
				Operation: &operations.PowerControlTaskInfo{
					Operation: operations.PowerOperationForcePowerOff,
				},
				TargetStrategy:   strategy,
				ConflictStrategy: eventrule.ConflictStrategyQueue,
			},
		}}},
	}
}

type staticLeakReader struct {
	machines []string
}

func (r staticLeakReader) GetLeakingMachineIds(context.Context) ([]string, error) {
	return slices.Clone(r.machines), nil
}

func (staticLeakReader) GetLeakingSwitchIds(context.Context) ([]string, error) {
	return nil, nil
}

type leakageInventory struct {
	rack       *rack.Rack
	components map[uuid.UUID]*component.Component
}

func newLeakageInventory(rackID uuid.UUID, components []component.Component) *leakageInventory {
	resolvedRack := &rack.Rack{
		Info:       deviceinfo.DeviceInfo{ID: rackID},
		Components: slices.Clone(components),
	}

	byID := make(map[uuid.UUID]*component.Component, len(components))
	for i := range components {
		cloned := components[i]
		byID[cloned.Info.ID] = &cloned
	}

	return &leakageInventory{rack: resolvedRack, components: byID}
}

func (i *leakageInventory) GetComponentByID(
	_ context.Context,
	id uuid.UUID,
) (*component.Component, error) {
	stored, ok := i.components[id]
	if !ok {
		return nil, fmt.Errorf("component %q not found", id)
	}

	resolved := *stored

	return &resolved, nil
}

func (i *leakageInventory) GetComponentsByExternalIDs(
	_ context.Context,
	externalIDs []string,
) ([]*component.Component, error) {
	requested := make(map[string]struct{}, len(externalIDs))
	for _, externalID := range externalIDs {
		requested[externalID] = struct{}{}
	}

	var resolved []*component.Component
	for _, stored := range i.components {
		if _, ok := requested[stored.ComponentID]; !ok {
			continue
		}

		cloned := *stored
		resolved = append(resolved, &cloned)
	}

	return resolved, nil
}

func (i *leakageInventory) GetRackByIdentifier(
	_ context.Context,
	_ identifier.Identifier,
	_ bool,
) (*rack.Rack, error) {
	cloned := *i.rack
	cloned.Components = slices.Clone(i.rack.Components)

	return &cloned, nil
}

func leakageComponent(
	id uuid.UUID,
	rackID uuid.UUID,
	externalID string,
	slot int,
) component.Component {
	return component.Component{
		Type:        devicetypes.ComponentTypeCompute,
		Info:        deviceinfo.DeviceInfo{ID: id},
		Position:    component.InRackPosition{SlotID: slot},
		ComponentID: externalID,
		RackID:      rackID,
	}
}

type recordingTaskManager struct {
	mu       sync.Mutex
	requests []*operation.Request
}

func (m *recordingTaskManager) SubmitTask(
	_ context.Context,
	request *operation.Request,
) ([]uuid.UUID, error) {
	m.mu.Lock()
	defer m.mu.Unlock()

	m.requests = append(m.requests, request)

	return []uuid.UUID{uuid.New()}, nil
}

func (m *recordingTaskManager) count() int {
	m.mu.Lock()
	defer m.mu.Unlock()

	return len(m.requests)
}

func (m *recordingTaskManager) componentIDs() []uuid.UUID {
	m.mu.Lock()
	defer m.mu.Unlock()

	ids := make([]uuid.UUID, 0, len(m.requests[0].TargetSpec.Components))
	for _, target := range m.requests[0].TargetSpec.Components {
		ids = append(ids, target.UUID)
	}

	slices.SortFunc(ids, compareUUID)

	return ids
}

func compareUUID(a, b uuid.UUID) int {
	return slices.Compare(a[:], b[:])
}
