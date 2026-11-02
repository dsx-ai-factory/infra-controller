// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
)

func TestDpuExtensionServiceFromStandardDpuTarget(t *testing.T) {
	target := standard.DPUEXTENSIONSERVICEDPUTARGET_ALL_ACTIVE
	apiService := standard.DpuExtensionService{}
	apiService.DpuTarget.Set(&target)

	service := dpuExtensionServiceFromStandard(apiService)
	require.NotNil(t, service.DpuTarget)
	assert.Equal(t, DpuExtensionServiceDpuTargetAllActive, *service.DpuTarget)
}
