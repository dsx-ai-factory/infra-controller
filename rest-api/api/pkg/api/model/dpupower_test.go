// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestAPIDpuPowerControlRequestValidate(t *testing.T) {
	require.NoError(t, (&APIDpuPowerControlRequest{Action: MachinePowerActionGracefulRestart}).Validate())
	assert.Error(t, (&APIDpuPowerControlRequest{Action: MachinePowerActionForceRestart}).Validate())
	assert.Error(t, (&APIDpuPowerControlRequest{}).Validate())
}

func TestAPIDpuPowerControlRequestToProto(t *testing.T) {
	request := (&APIDpuPowerControlRequest{Action: MachinePowerActionGracefulRestart}).ToProto("dpu-1")
	assert.Equal(t, "dpu-1", request.GetMachineId())
	assert.Equal(t, "GracefulRestart", request.GetAction().String())
}
