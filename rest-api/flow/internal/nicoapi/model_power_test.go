// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nicoapi

import (
	"testing"

	"github.com/stretchr/testify/require"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestMachinePowerStateFromPb(t *testing.T) {
	for _, tc := range []struct {
		name   string
		actual corev1.PowerState
		want   PowerState
	}{
		{"on", corev1.PowerState_On, PowerStateOn},
		{"off", corev1.PowerState_Off, PowerStateOff},
		{"disabled observation is unknown", corev1.PowerState_PowerManagerDisabled, PowerStateUnknown},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got := machinePowerStateFromPb(&corev1.PowerOptions{
				HostId:      &corev1.MachineId{Id: "host-1"},
				ActualState: tc.actual,
			})
			require.Equal(t, MachinePowerState{MachineID: "host-1", PowerState: tc.want}, got)
		})
	}
}
