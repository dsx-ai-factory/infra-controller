// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nicoapi

import (
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/types/known/timestamppb"

	corev1 "github.com/dsx-ai-factory/infra-controller/rest-api/proto/core/gen/v1"
)

func TestMachineDetailFromPbIncludesAssociatedDPUs(t *testing.T) {
	machine := &corev1.Machine{
		Id:          &corev1.MachineId{Id: "host-1"},
		MachineType: corev1.MachineType_HOST,
		Status: &corev1.MachineStatus{
			AssociatedDpuMachineIds: []*corev1.MachineId{
				{Id: "dpu-1"},
				{Id: ""},
				{Id: "dpu-2"},
			},
		},
	}

	detail := machineDetailFromPb(machine)

	assert.Equal(t, []string{"dpu-1", "dpu-2"}, detail.AssociatedDpuMachineIDs)
}

func TestMachineDetailFromPbIncludesHealth(t *testing.T) {
	observedAt := time.Date(2026, time.September, 28, 12, 0, 0, 0, time.UTC)
	inAlertSince := observedAt.Add(-time.Minute)
	target := "PSU1"
	tenantMessage := "replace the power supply"
	machine := &corev1.Machine{
		Id: &corev1.MachineId{Id: "host-1"},
		Status: &corev1.MachineStatus{Health: &corev1.HealthReport{
			Source:     "aggregate-host-health",
			ObservedAt: timestamppb.New(observedAt),
			Successes:  []*corev1.HealthProbeSuccess{{Id: "FanSpeed"}},
			Alerts: []*corev1.HealthProbeAlert{{
				Id:              "PowerSupply",
				Target:          &target,
				InAlertSince:    timestamppb.New(inAlertSince),
				Message:         "fault",
				TenantMessage:   &tenantMessage,
				Classifications: []string{"PreventAllocations"},
			}},
		}},
	}

	detail := machineDetailFromPb(machine)

	require.NotNil(t, detail.Health)
	assert.Equal(t, "aggregate-host-health", detail.Health.Source)
	assert.Equal(t, observedAt, *detail.Health.ObservedAt)
	assert.Equal(t, "FanSpeed", detail.Health.Successes[0].ID)
	assert.Equal(t, "PowerSupply", detail.Health.Alerts[0].ID)
	assert.Equal(t, target, *detail.Health.Alerts[0].Target)
	assert.Equal(t, inAlertSince, *detail.Health.Alerts[0].InAlertSince)
	assert.Equal(t, tenantMessage, *detail.Health.Alerts[0].TenantMessage)
}
