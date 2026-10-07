// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// APIMachineBMCResetRequest represents a request to reset the BMC of a Machine
type APIMachineBMCResetRequest struct {
	UseIpmiTool bool `json:"useIpmiTool"`
	// AcknowledgeAttachedInstance indicates the caller is aware that an Instance is currently attached to the Machine.
	AcknowledgeAttachedInstance *bool `json:"acknowledgeAttachedInstance"`
}

// ToProto converts the APIMachineBMCResetRequest to a Core gRPC AdminBmcResetRequest
func (ambrr *APIMachineBMCResetRequest) ToProto(machineID string) *corev1.AdminBmcResetRequest {
	return &corev1.AdminBmcResetRequest{
		MachineId:   &machineID,
		UseIpmitool: ambrr.UseIpmiTool,
	}
}
