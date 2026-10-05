// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package leakdetection

import (
	"context"
	"fmt"

	"github.com/rs/zerolog/log"
	"github.com/uptrace/bun"

	cdb "github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/db"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/db/model"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/operation"
	taskmanager "github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/task/manager"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/pkg/types"
)

// Query core to get IDs of leaking machines and submit power-off tasks for each
func runLeakDetectionOneMachine(
	ctx context.Context,
	nicoClient nicoapi.Client,
	taskMgr taskmanager.Manager,
	pool *cdb.Session,
) {
	log.Info().Msg("Running leak detection for machines")

	leakingMachineIds, err := nicoClient.GetLeakingMachineIds(ctx)
	if err != nil {
		log.Error().Err(err).Msg("Unable to retrieve leaking machine IDs from NICo")
		return
	}

	log.Info().Msgf("Found %d leaking machine IDs", len(leakingMachineIds))

	for _, machineID := range leakingMachineIds {
		log.Info().Msgf("Leaking machine ID: %s, submitting force power-off task", machineID)

		err := submitPowerOffTask(ctx, taskMgr, machineID, devicetypes.ComponentTypeCompute)
		if err != nil {
			log.Error().Err(err).Str("machine_id", machineID).
				Msg("Failed to submit power-off task for leaking machine")
		}
	}

	reconcileLeakStatus(ctx, pool, devicetypes.ComponentTypeCompute, leakingMachineIds)
}

// Query core to get IDs of leaking switches and submit power-off tasks for each
func runLeakDetectionOneSwitch(
	ctx context.Context,
	nicoClient nicoapi.Client,
	taskMgr taskmanager.Manager,
	pool *cdb.Session,
) {
	log.Info().Msg("Running leak detection for switches")

	leakingSwitchIds, err := nicoClient.GetLeakingSwitchIds(ctx)
	if err != nil {
		log.Error().Err(err).Msg("Unable to retrieve leaking switch IDs from NICo")
		return
	}

	log.Info().Msgf("Found %d leaking switch IDs", len(leakingSwitchIds))

	for _, switchID := range leakingSwitchIds {
		log.Info().Msgf("Leaking switch ID: %s, submitting force power-off task", switchID)

		err := submitPowerOffTask(ctx, taskMgr, switchID, devicetypes.ComponentTypeNVSwitch)
		if err != nil {
			log.Error().Err(err).Str("switch_id", switchID).
				Msg("Failed to submit power-off task for leaking switch")
		}
	}

	reconcileLeakStatus(ctx, pool, devicetypes.ComponentTypeNVSwitch, leakingSwitchIds)
}

func runLeakDetectionOne(
	ctx context.Context,
	nicoClient nicoapi.Client,
	taskMgr taskmanager.Manager,
	pool *cdb.Session,
) {
	log.Info().Msg("Running leak detection")

	runLeakDetectionOneMachine(ctx, nicoClient, taskMgr, pool)
	runLeakDetectionOneSwitch(ctx, nicoClient, taskMgr, pool)
}

// reconcileLeakStatus writes the per-component leak_status for every known
// component of the given type: DETECTED for the external IDs core flagged
// this cycle, NOT_DETECTED for the rest. Components without an external_id
// (not yet matched to a core machine/switch) keep their resting UNKNOWN.
// Only changed rows are written, mirroring persistComponentStatuses in the
// inventory loop. A nil pool (test wiring without a database) is a no-op.
func reconcileLeakStatus(
	ctx context.Context,
	pool *cdb.Session,
	componentType devicetypes.ComponentType,
	leakingIDs []string,
) {
	if pool == nil {
		return
	}

	components, err := model.GetComponentsByType(ctx, pool.DB, componentType)
	if err != nil {
		log.Error().Err(err).
			Str("component_type", componentType.String()).
			Msg("Leak detection: unable to load components to reconcile leak status")
		return
	}

	leaking := make(map[string]struct{}, len(leakingIDs))
	for _, id := range leakingIDs {
		leaking[id] = struct{}{}
	}

	var toUpdate []model.Component
	for i := range components {
		comp := &components[i]
		if comp.ComponentID == nil || *comp.ComponentID == "" {
			continue
		}
		desired := types.LeakStatusNotDetected
		if _, ok := leaking[*comp.ComponentID]; ok {
			desired = types.LeakStatusDetected
		}
		if comp.LeakStatus == desired {
			continue
		}
		comp.LeakStatus = desired
		toUpdate = append(toUpdate, *comp)
	}

	if len(toUpdate) == 0 {
		return
	}

	if err := pool.RunInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		for _, comp := range toUpdate {
			if err := comp.SetLeakStatusByComponentID(ctx, tx); err != nil {
				return fmt.Errorf("set leak status: %w", err)
			}
		}
		return nil
	}); err != nil {
		log.Error().Err(err).
			Str("component_type", componentType.String()).
			Msg("Leak detection: unable to persist component leak statuses")
	}
}

func submitPowerOffTask(
	ctx context.Context,
	taskMgr taskmanager.Manager,
	componentExternalId string,
	componentType devicetypes.ComponentType,
) error {
	info := &operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationForcePowerOff,
		Forced:    true,
	}

	raw, err := info.Marshal()
	if err != nil {
		return fmt.Errorf("failed to marshal power control info: %w", err)
	}

	req := &operation.Request{
		Operation: operation.Wrapper{
			Type: info.Type(),
			Code: info.CodeString(),
			Info: raw,
		},
		TargetSpec: operation.TargetSpec{
			Components: []operation.ComponentTarget{
				{
					External: &operation.ExternalRef{
						Type: componentType,
						ID:   componentExternalId,
					},
				},
			},
		},
		Description:      fmt.Sprintf("Leak detection: force power-off component %s of type %s", componentExternalId, componentType.String()),
		ConflictStrategy: operation.ConflictStrategyQueue,
	}

	taskIDs, err := taskMgr.SubmitTask(ctx, req)
	if err != nil {
		return fmt.Errorf("failed to submit task: %w", err)
	}

	if len(taskIDs) == 0 {
		return fmt.Errorf("failed to create any power-off tasks for leaking component %s of type %s", componentExternalId, componentType.String())
	}

	log.Info().
		Str("component_external_id", componentExternalId).
		Str("component_type", componentType.String()).
		Int("task_count", len(taskIDs)).
		Msg("Power-off task submitted for leaking component")

	return nil
}
