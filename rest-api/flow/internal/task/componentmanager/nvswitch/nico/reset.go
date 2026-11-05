// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nico

import (
	"context"
	"fmt"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"go.temporal.io/sdk/temporal"
)

// ResetNVOS submits once after the rack-host readiness gate. Success only means
// accepted and tracked. No status lookup follows any unsuccessful submission.
func (m *Manager) ResetNVOS(ctx context.Context, target common.Target) error {
	if m.nicoClient == nil {
		return fmt.Errorf("nico client is not configured")
	}
	if target.UsesMACAddresses() || target.Len() != 1 {
		return fmt.Errorf("NVOS reset requires one ingested switch")
	}
	err := m.ensureTargetOperable(ctx, target, types.OperationTypePowerControl, false)
	if err != nil {
		return err
	}
	response, err := m.nicoClient.ComponentReset(ctx, &corev1.ComponentResetRequest{
		Target: &corev1.ComponentResetRequest_SwitchIds{SwitchIds: switchIDsProto(target.Identifiers)},
		Action: corev1.ComponentResetAction_COMPONENT_RESET_ACTION_NVOS_FACTORY_RESET,
	})
	if err != nil {
		return fmt.Errorf("NVOS reset submission failed or its outcome is unknown; do not automatically resubmit: %w", err)
	}
	if response == nil || len(response.Results) != 1 {
		return fmt.Errorf("NVOS reset submission returned no attributable result; outcome is unknown")
	}
	result := response.Results[0]
	if result == nil || result.GetComponentId() != target.Identifiers[0] {
		return fmt.Errorf("NVOS reset submission returned a mismatched identity; outcome is unknown")
	}
	if result.Status != corev1.ComponentManagerStatusCode_COMPONENT_MANAGER_STATUS_CODE_SUCCESS {
		if result.Status == corev1.ComponentManagerStatusCode_COMPONENT_MANAGER_STATUS_CODE_OUTCOME_UNKNOWN {
			return fmt.Errorf("NVOS reset submission outcome is unknown; do not automatically resubmit")
		}
		return temporal.NewNonRetryableApplicationError(fmt.Sprintf("NVOS reset submission status %s; do not automatically resubmit", result.Status), "NVOSResetRejected", nil)
	}
	return nil
}

// NVOSResetCompleted reads only after this workflow observed a successful submit.
// Core tracks the latest accepted reference; external submissions may replace it.
func (m *Manager) NVOSResetCompleted(ctx context.Context, target common.Target) (bool, error) {
	if m.nicoClient == nil {
		return fmt.Errorf("nico client is not configured")
	}
	if target.UsesMACAddresses() || target.Len() != 1 {
		return false, fmt.Errorf("NVOS reset requires one ingested switch")
	}
	response, err := m.nicoClient.GetComponentResetStatus(ctx, &corev1.GetComponentResetStatusRequest{
		Target: &corev1.GetComponentResetStatusRequest_SwitchIds{SwitchIds: switchIDsProto(target.Identifiers)},
		Action: corev1.ComponentResetAction_COMPONENT_RESET_ACTION_NVOS_FACTORY_RESET,
	})
	if err != nil {
		return false, err
	}
	if response == nil || len(response.Statuses) != 1 {
		return false, fmt.Errorf("NVOS reset status is unavailable")
	}
	observed := response.Statuses[0]
	if observed == nil || observed.Result == nil || observed.Result.GetComponentId() != target.Identifiers[0] || observed.Action != corev1.ComponentResetAction_COMPONENT_RESET_ACTION_NVOS_FACTORY_RESET {
		return false, fmt.Errorf("NVOS reset status returned a mismatched identity or action")
	}
	if observed.Result.Status != corev1.ComponentManagerStatusCode_COMPONENT_MANAGER_STATUS_CODE_SUCCESS {
		return false, fmt.Errorf("NVOS reset observation status %s", observed.Result.Status)
	}
	switch observed.State {
	case corev1.ComponentResetState_COMPONENT_RESET_STATE_QUEUED, corev1.ComponentResetState_COMPONENT_RESET_STATE_IN_PROGRESS:
		return false, nil
	case corev1.ComponentResetState_COMPONENT_RESET_STATE_COMPLETED:
		return true, nil
	case corev1.ComponentResetState_COMPONENT_RESET_STATE_FAILED:
		return false, temporal.NewNonRetryableApplicationError("NVOS factory reset failed", "NVOSResetFailed", nil)
	default:
		return false, fmt.Errorf("NVOS reset outcome is unknown")
	}
}
