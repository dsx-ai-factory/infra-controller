// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestAPIMachineBMCResetRequestToProto(t *testing.T) {
	req := APIMachineBMCResetRequest{UseIpmiTool: true}

	protoReq := req.ToProto("machine-1")
	assert.Equal(t, "machine-1", protoReq.GetMachineId())
	assert.True(t, protoReq.GetUseIpmitool())
}
