// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nicoapi

import (
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/types/known/timestamppb"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestMachineDetailFromPb(t *testing.T) {
	tests := []struct {
		name string
		run  func(*testing.T)
	}{
		{
			name: "associated DPUs",
			run: func(t *testing.T) {
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
			},
		},
		{
			name: "health",
			run: func(t *testing.T) {
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
			},
		},
		{
			name: "BMC compatibility",
			run:  testMachineBMCCompatibility,
		},
	}
	for _, test := range tests {
		t.Run(test.name, test.run)
	}
}

//nolint:staticcheck // Legacy response fixtures verify compatibility before the BMC split.
func testMachineBMCCompatibility(t *testing.T) {
	t.Helper()
	ip := "192.0.2.10"
	mac := "02:00:00:00:00:10"
	firmware := "new-firmware"
	legacyIP := "192.0.2.20"
	legacyMac := "02:00:00:00:00:20"
	legacyFirmware := "legacy-firmware"
	endpoint := &corev1.BmcEndpoint{
		Ip:  &ip,
		Mac: &mac,
	}
	status := &corev1.MachineStatus{
		BmcStatus: &corev1.BmcStatus{
			FirmwareVersion: &firmware,
		},
	}
	legacy := &corev1.BmcInfo{
		Ip:              &legacyIP,
		Mac:             &legacyMac,
		FirmwareVersion: &legacyFirmware,
	}
	tests := []struct {
		name   string
		bmc    *corev1.BmcEndpoint
		status *corev1.MachineStatus
		legacy *corev1.BmcInfo
		want   []string
	}{
		{
			name:   "new Core without legacy fields",
			bmc:    endpoint,
			status: status,
			want:   []string{ip, mac, firmware},
		},
		{
			name:   "older Core with only legacy fields",
			legacy: legacy,
			want:   []string{legacyIP, legacyMac, legacyFirmware},
		},
		{
			name:   "new fields take precedence over legacy fields",
			bmc:    endpoint,
			status: status,
			legacy: legacy,
			want:   []string{ip, mac, firmware},
		},
		{
			name: "unknown new values do not use legacy values",
			bmc:  &corev1.BmcEndpoint{},
			status: &corev1.MachineStatus{
				BmcStatus: &corev1.BmcStatus{},
			},
			legacy: legacy,
			want:   []string{"", "", ""},
		},
		{
			name:   "endpoint and firmware fall back independently",
			bmc:    endpoint,
			status: &corev1.MachineStatus{},
			legacy: legacy,
			want:   []string{ip, mac, legacyFirmware},
		},
		{
			name:   "firmware and endpoint fall back independently",
			status: status,
			legacy: legacy,
			want:   []string{legacyIP, legacyMac, firmware},
		},
		{
			name: "missing BMC data and status",
			want: []string{"", "", ""},
		},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			machine := &corev1.Machine{
				Id:      &corev1.MachineId{Id: "host-1"},
				Bmc:     test.bmc,
				Status:  test.status,
				BmcInfo: test.legacy,
			}
			detail := machineDetailFromPb(machine)
			assert.Equal(t, test.want, []string{detail.BmcIP, detail.BmcMac, detail.FirmwareVersion})
		})
	}
}
