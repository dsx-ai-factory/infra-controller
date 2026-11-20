// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestAPIDpuMachinePowerUpdateRequest_Validate(t *testing.T) {
	const siteID = "5a0c3f0e-6b0e-4d7c-9a54-1f3b8d1c2e77"

	tests := []struct {
		name string
		req  APIDpuMachinePowerUpdateRequest
		// wantField is the JSON name of the field expected to fail; empty means valid.
		wantField string
	}{
		{name: "accepts a Site ID and action", req: APIDpuMachinePowerUpdateRequest{SiteID: siteID, Action: MachinePowerActionForceRestart}},
		{name: "requires a Site ID", req: APIDpuMachinePowerUpdateRequest{Action: MachinePowerActionOn}, wantField: "siteId"},
		{name: "requires a UUID Site ID", req: APIDpuMachinePowerUpdateRequest{SiteID: "site-1", Action: MachinePowerActionOn}, wantField: "siteId"},
		{name: "requires an action", req: APIDpuMachinePowerUpdateRequest{SiteID: siteID}, wantField: "action"},
		{name: "rejects an action outside the Machine power set", req: APIDpuMachinePowerUpdateRequest{SiteID: siteID, Action: "PowerCycle"}, wantField: "action"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if tt.wantField == "" {
				assert.NoError(t, err)
				return
			}
			var validationErrs validation.Errors
			require.ErrorAs(t, err, &validationErrs)
			assert.Len(t, validationErrs, 1)
			assert.Contains(t, validationErrs, tt.wantField)
		})
	}
}

func TestAPIDpuMachinePowerUpdateRequest_ToProto(t *testing.T) {
	tests := []struct {
		name       string
		action     MachinePowerAction
		wantAction corev1.AdminPowerControlRequest_SystemPowerControl
	}{
		{name: "On", action: MachinePowerActionOn, wantAction: corev1.AdminPowerControlRequest_On},
		{name: "GracefulShutdown", action: MachinePowerActionGracefulShutdown, wantAction: corev1.AdminPowerControlRequest_GracefulShutdown},
		{name: "ForceOff", action: MachinePowerActionForceOff, wantAction: corev1.AdminPowerControlRequest_ForceOff},
		{name: "GracefulRestart", action: MachinePowerActionGracefulRestart, wantAction: corev1.AdminPowerControlRequest_GracefulRestart},
		{name: "ForceRestart", action: MachinePowerActionForceRestart, wantAction: corev1.AdminPowerControlRequest_ForceRestart},
		{name: "ACPowercycle", action: MachinePowerActionACPowercycle, wantAction: corev1.AdminPowerControlRequest_ACPowercycle},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			req := APIDpuMachinePowerUpdateRequest{
				SiteID:       "5a0c3f0e-6b0e-4d7c-9a54-1f3b8d1c2e77",
				Action:       tt.action,
				DpuMachineID: "fm100dsbiu5ckus880v8407u0mkcensa39cule26im5gnpvmuufckacguc0",
			}

			protoReq := req.ToProto()

			assert.Equal(t, "fm100dsbiu5ckus880v8407u0mkcensa39cule26im5gnpvmuufckacguc0", protoReq.GetMachineId())
			assert.Equal(t, tt.wantAction, protoReq.GetAction())
			assert.Nil(t, protoReq.BmcEndpointRequest)
		})
	}
}
