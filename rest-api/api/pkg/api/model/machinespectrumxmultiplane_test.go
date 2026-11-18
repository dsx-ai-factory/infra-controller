// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/emptypb"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestAPIMachineSpectrumXMultiplaneUpdateRequest_Validate(t *testing.T) {
	for _, tc := range []struct{ name, body string }{
		{"missing selector", `{"plane":0,"maintenanceMode":true}`},
		{"ambiguous selector", `{"rail":0,"allRails":true,"plane":0,"maintenanceMode":true}`},
		{"false all selector", `{"allRails":false,"plane":0,"maintenanceMode":true}`},
		{"missing mode", `{"rail":0,"plane":0}`},
		{"missing endpoint", `{"railRange":{"start":0},"plane":0,"maintenanceMode":true}`},
		{"descending range", `{"rail":0,"planeRange":{"start":3,"end":1},"maintenanceMode":true}`},
		{"rail bound", `{"railRange":{"start":0,"end":4},"plane":0,"maintenanceMode":true}`},
		{"plane bound", `{"rail":0,"plane":8,"maintenanceMode":true}`},
		{"negative index", `{"rail":-1,"plane":0,"maintenanceMode":true}`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var req APIMachineSpectrumXMultiplaneUpdateRequest
			require.NoError(t, json.Unmarshal([]byte(tc.body), &req))
			err := req.Validate()
			require.Error(t, err)
			assert.IsType(t, validation.Errors{}, err)
		})
	}
}

func TestAPIMachineSpectrumXMultiplaneUpdateRequest_ToProto(t *testing.T) {
	for _, tc := range []struct {
		name, body string
		want       *corev1.MachineSpxMultiplaneMaintenanceUpdateRequest
	}{
		{"zero and explicit false with clear reference", `{"rail":0,"plane":0,"maintenanceMode":false,"reference":""}`, &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest{
			RailSelector: &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest_Rail{Rail: 0}, PlaneSelector: &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest_Plane{Plane: 0}, MaintenanceMode: cutil.GetPtr(false), Reference: cutil.GetPtr(""),
		}},
		{"all with omitted reference", `{"allRails":true,"allPlanes":true,"maintenanceMode":true}`, &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest{
			RailSelector: &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest_AllRails{AllRails: &emptypb.Empty{}}, PlaneSelector: &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest_AllPlanes{AllPlanes: &emptypb.Empty{}}, MaintenanceMode: cutil.GetPtr(true),
		}},
		{"inclusive ranges", `{"railRange":{"start":0,"end":3},"planeRange":{"start":2,"end":7},"maintenanceMode":true,"reference":"CHG-12345"}`, &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest{
			RailSelector: &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest_RailRange{RailRange: &corev1.SpxMultiplaneRange{Start: cutil.GetPtr(uint32(0)), End: cutil.GetPtr(uint32(3))}}, PlaneSelector: &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest_PlaneRange{PlaneRange: &corev1.SpxMultiplaneRange{Start: cutil.GetPtr(uint32(2)), End: cutil.GetPtr(uint32(7))}}, MaintenanceMode: cutil.GetPtr(true), Reference: cutil.GetPtr("CHG-12345"),
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var req APIMachineSpectrumXMultiplaneUpdateRequest
			require.NoError(t, json.Unmarshal([]byte(tc.body), &req))
			require.NoError(t, req.Validate())
			req.MachineID = "machine-from-path"
			tc.want.MachineId = &corev1.MachineId{Id: req.MachineID}
			assert.True(t, proto.Equal(tc.want, req.ToProto()), "%v", req.ToProto())
		})
	}
}

func TestAPIMachineSpectrumXMultiplane_FromProto(t *testing.T) {
	var response APIMachineSpectrumXMultiplane
	response.FromProto(&corev1.MachineSpxMultiplane{
		MachineId:  &corev1.MachineId{Id: "machine"},
		Interfaces: []*corev1.MachineSpxMultiplaneInterface{{LogicalIfname: "eth_r0_p0"}},
		DpuStatuses: []*corev1.MachineSpxMultiplaneDpuStatus{
			{DpuMachineId: &corev1.MachineId{Id: "dpu-a"}, Interfaces: []*corev1.AstraMultiplaneInterfaceStatus{{LogicalIfname: "eth_r0_p0", MaintenanceMode: cutil.GetPtr(false)}}},
			{DpuMachineId: &corev1.MachineId{Id: "dpu-b"}, Interfaces: []*corev1.AstraMultiplaneInterfaceStatus{{LogicalIfname: "eth_r0_p0"}}},
		},
	})
	data, err := json.Marshal(response)
	require.NoError(t, err)
	assert.Nil(t, response.Interfaces[0].ConfiguredMaintenanceMode)
	assert.Equal(t, cutil.GetPtr(false), response.DPUStatuses[0].Interfaces[0].MaintenanceMode)
	assert.Nil(t, response.DPUStatuses[1].Interfaces[0].MaintenanceMode)
	assert.Contains(t, string(data), `"configuredMaintenanceMode":null`)
	assert.Contains(t, string(data), `"maintenanceMode":false`)
	assert.Contains(t, string(data), `"maintenanceMode":null`)
	assert.Contains(t, string(data), `"observed":null`)
	response.FromProto(&corev1.MachineSpxMultiplane{MachineId: &corev1.MachineId{Id: "machine"}})
	data, err = json.Marshal(response)
	require.NoError(t, err)
	assert.JSONEq(t, `{"machineId":"machine","interfaces":[],"dpuStatuses":[]}`, string(data))
}
