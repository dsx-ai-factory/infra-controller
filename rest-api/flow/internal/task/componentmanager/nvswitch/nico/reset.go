// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nico

import (
	"context"
	"fmt"
	"strings"
	"time"

	"go.temporal.io/sdk/temporal"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// NVOSResetCompleted reads only after this workflow observed a successful submit.
// Core tracks the latest accepted reference; external submissions may replace it.
func (m *Manager) NVOSResetCompleted(ctx context.Context, target common.Target) (bool, error) {
	return m.switchResetCompleted(ctx, target, corev1.ComponentResetAction_COMPONENT_RESET_ACTION_NVOS_FACTORY_RESET, "NVOS")
}

func (m *Manager) SDNResetCompleted(ctx context.Context, target common.Target) (bool, error) {
	return m.switchResetCompleted(ctx, target, corev1.ComponentResetAction_COMPONENT_RESET_ACTION_SDN_FACTORY_RESET, "SDN")
}

func (m *Manager) switchResetCompleted(ctx context.Context, target common.Target, action corev1.ComponentResetAction, scope string) (bool, error) {
	if m.nicoClient == nil {
		return false, fmt.Errorf("nico client is not configured")
	}
	if target.UsesMACAddresses() || target.Len() != 1 {
		return false, fmt.Errorf("%s reset requires one ingested switch", scope)
	}
	response, err := m.nicoClient.GetComponentResetStatus(ctx, &corev1.GetComponentResetStatusRequest{
		Target: &corev1.GetComponentResetStatusRequest_SwitchIds{SwitchIds: switchIDsProto(target.Identifiers)},
		Action: action,
	})
	if err != nil {
		return false, err
	}
	if response == nil || len(response.Statuses) != 1 {
		return false, fmt.Errorf("%s reset status is unavailable", scope)
	}
	observed := response.Statuses[0]
	if observed == nil || observed.Result == nil || observed.Result.GetComponentId() != target.Identifiers[0] || observed.Action != action {
		return false, fmt.Errorf("%s reset status returned a mismatched identity or action", scope)
	}
	if observed.Result.Status != corev1.ComponentManagerStatusCode_COMPONENT_MANAGER_STATUS_CODE_SUCCESS {
		return false, fmt.Errorf("%s", resetResultMessage(fmt.Sprintf("%s reset observation status %s", scope, observed.Result.Status), observed.Result))
	}
	switch observed.State {
	case corev1.ComponentResetState_COMPONENT_RESET_STATE_QUEUED, corev1.ComponentResetState_COMPONENT_RESET_STATE_IN_PROGRESS:
		return false, nil
	case corev1.ComponentResetState_COMPONENT_RESET_STATE_COMPLETED:
		return true, nil
	case corev1.ComponentResetState_COMPONENT_RESET_STATE_FAILED:
		return false, temporal.NewNonRetryableApplicationError(resetResultMessage(scope+" factory reset failed", observed.Result), scope+"ResetFailed", nil)
	default:
		return false, fmt.Errorf("%s reset outcome is unknown", scope)
	}
}

// PrepareFactoryReset waits once for all target switches' rack hosts.
func (m *Manager) PrepareFactoryReset(ctx context.Context, target common.Target) error {
	if m.nicoClient == nil || target.UsesMACAddresses() || target.Len() == 0 {
		return fmt.Errorf("factory reset requires ingested switches and a configured client")
	}
	return m.ensureTargetOperable(ctx, target, types.OperationTypePowerControl, false)
}

// SubmitFactoryReset submits one switch after a bounded readiness recheck.
// The preflight is not a reservation: inventory can change between submissions.
func (m *Manager) SubmitFactoryReset(ctx context.Context, target common.Target, scope string) error {
	if m.nicoClient == nil || target.UsesMACAddresses() || target.Len() != 1 {
		return fmt.Errorf("factory reset requires one ingested switch and a configured client")
	}
	var action corev1.ComponentResetAction
	switch scope {
	case "nvos":
		action = corev1.ComponentResetAction_COMPONENT_RESET_ACTION_NVOS_FACTORY_RESET
	case "sdn":
		action = corev1.ComponentResetAction_COMPONENT_RESET_ACTION_SDN_FACTORY_RESET
	default:
		return fmt.Errorf("unsupported factory reset target %q", scope)
	}
	scope = strings.ToUpper(scope)
	gateCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	err := m.ensureTargetOperable(gateCtx, target, types.OperationTypePowerControl, false)
	if err != nil {
		return temporal.NewNonRetryableApplicationError("factory reset readiness check failed", "FactoryResetNotReady", err)
	}
	response, err := m.nicoClient.ComponentReset(ctx, &corev1.ComponentResetRequest{
		Target: &corev1.ComponentResetRequest_SwitchIds{SwitchIds: switchIDsProto(target.Identifiers)},
		Action: action,
	})
	if err != nil {
		return fmt.Errorf("%s reset submission failed or its outcome is unknown; do not automatically resubmit: %w", scope, err)
	}
	if response == nil || len(response.Results) != 1 {
		return fmt.Errorf("%s reset submission returned no attributable result; outcome is unknown", scope)
	}
	result := response.Results[0]
	if result == nil || result.GetComponentId() != target.Identifiers[0] {
		return fmt.Errorf("%s reset submission returned a mismatched identity; outcome is unknown", scope)
	}
	if result.Status != corev1.ComponentManagerStatusCode_COMPONENT_MANAGER_STATUS_CODE_SUCCESS {
		if result.Status == corev1.ComponentManagerStatusCode_COMPONENT_MANAGER_STATUS_CODE_OUTCOME_UNKNOWN {
			return fmt.Errorf("%s", resetResultMessage(scope+" reset submission outcome is unknown; do not automatically resubmit", result))
		}
		return temporal.NewNonRetryableApplicationError(resetResultMessage(fmt.Sprintf("%s reset submission status %s; do not automatically resubmit", scope, result.Status), result), scope+"ResetRejected", nil)
	}
	return nil
}

// resetResultMessage keeps backend diagnostics without changing reset failure classification.
func resetResultMessage(summary string, result *corev1.ComponentResult) string {
	reason := strings.TrimSpace(result.GetError())
	if reason == "" {
		return summary
	}
	return summary + ": " + strings.Join(strings.Fields(reason), " ")
}
