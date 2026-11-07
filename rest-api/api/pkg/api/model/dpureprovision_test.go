// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestAPIMachineDpuReprovisionRequestValidateAndToProto(t *testing.T) {
	req := APIMachineDpuReprovisionRequest{Mode: MachineDpuReprovisionModeRestart, UpdateFirmware: true}
	require.NoError(t, req.Validate())

	protoReq := req.ToProto("machine-1")
	assert.Equal(t, "machine-1", protoReq.GetMachineId().GetId())
	assert.Equal(t, corev1.DpuReprovisioningRequest_Restart, protoReq.GetMode())
	assert.Equal(t, corev1.UpdateInitiator_AdminCli, protoReq.GetInitiator())
	assert.True(t, protoReq.GetUpdateFirmware())

	assert.Error(t, (&APIMachineDpuReprovisionRequest{}).Validate())
	assert.Error(t, (&APIMachineDpuReprovisionRequest{Mode: MachineDpuReprovisionMode("restart")}).Validate())
}
