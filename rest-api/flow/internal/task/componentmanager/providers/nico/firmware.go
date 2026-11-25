// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nico

import (
	"fmt"

	"google.golang.org/grpc/status"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// MapFirmwareState converts a NICo protobuf FirmwareUpdateState into the
// corresponding operations.FirmwareUpdateState.
func MapFirmwareState(state corev1.FirmwareUpdateState) operations.FirmwareUpdateState {
	switch state {
	case corev1.FirmwareUpdateState_FW_STATE_QUEUED:
		return operations.FirmwareUpdateStateQueued
	case corev1.FirmwareUpdateState_FW_STATE_IN_PROGRESS:
		return operations.FirmwareUpdateStateQueued // closest available state
	case corev1.FirmwareUpdateState_FW_STATE_VERIFYING:
		return operations.FirmwareUpdateStateVerifying
	case corev1.FirmwareUpdateState_FW_STATE_COMPLETED:
		return operations.FirmwareUpdateStateCompleted
	case corev1.FirmwareUpdateState_FW_STATE_FAILED, corev1.FirmwareUpdateState_FW_STATE_CANCELLED:
		return operations.FirmwareUpdateStateFailed
	default:
		return operations.FirmwareUpdateStateUnknown
	}
}

// FirmwareRPCError retains the RPC code without exposing upstream error text.
func FirmwareRPCError(action string, err error) error {
	return status.Error(status.Code(err), "credential-bearing firmware "+action+" failed")
}

// FirmwareResultDiagnostic identifies a rejected component without upstream text.
func FirmwareResultDiagnostic(result *corev1.ComponentResult, useMAC bool) string {
	return fmt.Sprintf("firmware operation for %s returned %s", ResultIdentifier(result, useMAC), result.GetStatus())
}
