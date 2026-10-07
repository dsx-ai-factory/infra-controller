// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package utils

import (
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
	"github.com/stretchr/testify/assert"
)

func TestSiteHealth(t *testing.T) {
	tests := []struct {
		name            string
		coreStatus      CompStatus
		temporalStatus  CompStatus
		flowGrpcEnabled bool
		flowStatus      CompStatus
		expected        CompStatus
	}{
		{
			name:            "all required dependencies healthy with Flow disabled",
			coreStatus:      CompHealthy,
			temporalStatus:  CompHealthy,
			flowGrpcEnabled: false,
			flowStatus:      CompUnhealthy,
			expected:        CompHealthy,
		},
		{
			name:            "all enabled dependencies healthy",
			coreStatus:      CompHealthy,
			temporalStatus:  CompHealthy,
			flowGrpcEnabled: true,
			flowStatus:      CompHealthy,
			expected:        CompHealthy,
		},
		{
			name:            "enabled Flow unhealthy",
			coreStatus:      CompHealthy,
			temporalStatus:  CompHealthy,
			flowGrpcEnabled: true,
			flowStatus:      CompUnhealthy,
			expected:        CompUnhealthy,
		},
		{
			name:            "enabled Flow not initialized",
			coreStatus:      CompHealthy,
			temporalStatus:  CompHealthy,
			flowGrpcEnabled: true,
			flowStatus:      CompNotKnown,
			expected:        CompUnhealthy,
		},
		{
			name:            "Core unhealthy",
			coreStatus:      CompUnhealthy,
			temporalStatus:  CompHealthy,
			flowGrpcEnabled: false,
			flowStatus:      CompHealthy,
			expected:        CompUnhealthy,
		},
		{
			name:            "Temporal unhealthy",
			coreStatus:      CompHealthy,
			temporalStatus:  CompUnhealthy,
			flowGrpcEnabled: false,
			flowStatus:      CompHealthy,
			expected:        CompUnhealthy,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			elektra := elektratypes.NewElektraTypes()
			elektra.Conf.FlowGrpc.Enabled = test.flowGrpcEnabled
			elektra.Managers.CoreGrpc.State.HealthStatus.Store(uint64(test.coreStatus))
			elektra.Managers.Workflow.State.HealthStatus.Store(uint64(test.temporalStatus))
			elektra.Managers.FlowGrpc.State.HealthStatus.Store(uint64(test.flowStatus))

			assert.Equal(t, test.expected, SiteHealth(elektra))
		})
	}
}
