// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestAPIMachineGPUStats_JSON(t *testing.T) {
	stats := APIMachineGPUStats{
		Name:     "NVIDIA H100 SXM5 80GB",
		GPUs:     92,
		Machines: 12,
	}

	data, err := json.Marshal(stats)
	require.Nil(t, err)

	var parsed map[string]interface{}
	err = json.Unmarshal(data, &parsed)
	require.Nil(t, err)

	assert.Equal(t, "NVIDIA H100 SXM5 80GB", parsed["name"])
	assert.Equal(t, float64(92), parsed["gpus"])
	assert.Equal(t, float64(12), parsed["machines"])

	var roundTrip APIMachineGPUStats
	err = json.Unmarshal(data, &roundTrip)
	require.Nil(t, err)
	assert.Equal(t, stats, roundTrip)
}

func TestAPIMachineGPUStats_ZeroValues(t *testing.T) {
	stats := APIMachineGPUStats{}

	data, err := json.Marshal(stats)
	require.Nil(t, err)

	var parsed map[string]interface{}
	err = json.Unmarshal(data, &parsed)
	require.Nil(t, err)

	assert.Equal(t, "", parsed["name"])
	assert.Equal(t, float64(0), parsed["gpus"])
	assert.Equal(t, float64(0), parsed["machines"])
}

func TestAPIMachineStatusBreakdown_JSON(t *testing.T) {
	bd := APIMachineStatusBreakdown{
		Total:        12,
		Initializing: 1,
		Ready:        5,
		InUse:        2,
		Error:        1,
		Maintenance:  1,
		Unknown:      1,
	}

	data, err := json.Marshal(bd)
	require.Nil(t, err)

	var parsed map[string]interface{}
	err = json.Unmarshal(data, &parsed)
	require.Nil(t, err)

	assert.Equal(t, float64(12), parsed["total"])
	assert.Equal(t, float64(1), parsed["initializing"])
	assert.Equal(t, float64(5), parsed["ready"])
	assert.Equal(t, float64(2), parsed["inUse"])
	assert.Equal(t, float64(1), parsed["error"])
	assert.Equal(t, float64(1), parsed["maintenance"])
	assert.Equal(t, float64(1), parsed["unknown"])

	var roundTrip APIMachineStatusBreakdown
	err = json.Unmarshal(data, &roundTrip)
	require.Nil(t, err)
	assert.Equal(t, bd, roundTrip)
}

func TestAPIMachineInstanceTypeSummary_JSON(t *testing.T) {
	summary := APIMachineInstanceTypeSummary{
		Assigned: APIMachineStatusBreakdown{
			Total: 29, Initializing: 2, Ready: 12, InUse: 6, Error: 3, Maintenance: 2, Unknown: 2,
		},
		Unassigned: APIMachineStatusBreakdown{
			Total: 8, Ready: 2, Error: 1, Maintenance: 1, Unknown: 1,
		},
	}

	data, err := json.Marshal(summary)
	require.Nil(t, err)

	var parsed map[string]interface{}
	err = json.Unmarshal(data, &parsed)
	require.Nil(t, err)

	assigned := parsed["assigned"].(map[string]interface{})
	assert.Equal(t, float64(29), assigned["total"])
	assert.Equal(t, float64(12), assigned["ready"])

	unassigned := parsed["unassigned"].(map[string]interface{})
	assert.Equal(t, float64(8), unassigned["total"])
	assert.Equal(t, float64(1), unassigned["unknown"])

	var roundTrip APIMachineInstanceTypeSummary
	err = json.Unmarshal(data, &roundTrip)
	require.Nil(t, err)
	assert.Equal(t, summary, roundTrip)
}

func TestAPIMachineInstanceTypeStats_JSON(t *testing.T) {
	stats := APIMachineInstanceTypeStats{
		ID:   "it-gpu-large",
		Name: "gpu-large",
		AssignedMachineStats: APIMachineStatusBreakdown{
			Total: 12, Initializing: 1, Ready: 5, InUse: 2, Error: 1, Maintenance: 1, Unknown: 1,
		},
		Allocated:      7,
		MaxAllocatable: 2,
		UsedMachineStats: APIMachineStatusBreakdown{
			Total: 4, InUse: 2, Error: 1, Maintenance: 1,
		},
		Tenants: []APIMachineInstanceTypeTenantStats{
			{
				ID:        "t-alpha",
				Name:      "alpha-org",
				Allocated: 6,
				UsedMachineStats: APIMachineStatusBreakdown{
					Total: 3, InUse: 2, Error: 1,
				},
				Allocations: []APIMachineInstanceTypeTenantAllocationStats{
					{ID: "a-1", Name: "training-reserved", Allocated: 4},
					{ID: "a-2", Name: "inference-ondemand", Allocated: 2},
				},
			},
			{
				ID:        "t-beta",
				Name:      "beta-org",
				Allocated: 1,
				UsedMachineStats: APIMachineStatusBreakdown{
					Total: 1, Maintenance: 1,
				},
				Allocations: []APIMachineInstanceTypeTenantAllocationStats{
					{ID: "a-3", Name: "simulation-pool", Allocated: 1},
				},
			},
		},
	}

	data, err := json.Marshal(stats)
	require.Nil(t, err)

	var parsed map[string]interface{}
	err = json.Unmarshal(data, &parsed)
	require.Nil(t, err)

	assert.Equal(t, "it-gpu-large", parsed["id"])
	assert.Equal(t, "gpu-large", parsed["name"])
	assert.Equal(t, float64(7), parsed["allocated"])
	assert.Equal(t, float64(2), parsed["maxAllocatable"])

	assignedStats := parsed["assignedMachineStats"].(map[string]interface{})
	assert.Equal(t, float64(12), assignedStats["total"])
	assert.Equal(t, float64(5), assignedStats["ready"])
	assert.Equal(t, float64(2), assignedStats["inUse"])
	assert.Equal(t, float64(1), assignedStats["error"])
	assert.Equal(t, float64(1), assignedStats["maintenance"])

	usedStats := parsed["usedMachineStats"].(map[string]interface{})
	assert.Equal(t, float64(4), usedStats["total"])
	assert.Equal(t, float64(2), usedStats["inUse"])
	assert.Equal(t, float64(1), usedStats["error"])
	assert.Equal(t, float64(1), usedStats["maintenance"])

	tenants := parsed["tenants"].([]interface{})
	assert.Equal(t, 2, len(tenants))

	alphaTenant := tenants[0].(map[string]interface{})
	assert.Equal(t, "alpha-org", alphaTenant["name"])
	assert.Equal(t, float64(6), alphaTenant["allocated"])

	alphaUsed := alphaTenant["usedMachineStats"].(map[string]interface{})
	assert.Equal(t, float64(3), alphaUsed["total"])
	assert.Equal(t, float64(2), alphaUsed["inUse"])

	allocs := alphaTenant["allocations"].([]interface{})
	assert.Equal(t, 2, len(allocs))
	assert.Equal(t, "training-reserved", allocs[0].(map[string]interface{})["name"])
	assert.Equal(t, float64(4), allocs[0].(map[string]interface{})["allocated"])

	var roundTrip APIMachineInstanceTypeStats
	err = json.Unmarshal(data, &roundTrip)
	require.Nil(t, err)
	assert.Equal(t, stats, roundTrip)
}

func TestAPIMachineInstanceTypeStats_EmptyTenants(t *testing.T) {
	stats := APIMachineInstanceTypeStats{
		ID:                   "it-456",
		Name:                 "storage.hdd",
		AssignedMachineStats: APIMachineStatusBreakdown{Total: 3, Ready: 3},
		MaxAllocatable:       3,
		Tenants:              nil,
	}

	data, err := json.Marshal(stats)
	require.Nil(t, err)

	var parsed map[string]interface{}
	err = json.Unmarshal(data, &parsed)
	require.Nil(t, err)

	assert.Nil(t, parsed["tenants"])
	assert.Equal(t, float64(3), parsed["maxAllocatable"])
}

func TestAPITenantInstanceTypeStats_JSON(t *testing.T) {
	stats := APITenantInstanceTypeStats{
		ID:             "t-alpha",
		Org:            "alpha-org",
		OrgDisplayName: "Alpha Corp",
		InstanceTypes: []APITenantInstanceTypeStatsEntry{
			{
				ID:        "it-gpu-large",
				Name:      "gpu-large",
				Allocated: 6,
				UsedMachineStats: APIMachineStatusBreakdown{
					Total: 3, InUse: 2, Error: 1,
				},
				MaxAllocatable: 2,
				Allocations: []APITenantInstanceTypeAllocationStats{
					{ID: "a-1", Name: "training-reserved", Total: 4},
					{ID: "a-2", Name: "inference-ondemand", Total: 2},
				},
			},
			{
				ID:        "it-gpu-small",
				Name:      "gpu-small",
				Allocated: 3,
				UsedMachineStats: APIMachineStatusBreakdown{
					Total: 2, InUse: 2,
				},
				MaxAllocatable: 1,
				Allocations: []APITenantInstanceTypeAllocationStats{
					{ID: "a-1", Name: "training-reserved", Total: 3},
				},
			},
		},
	}

	data, err := json.Marshal(stats)
	require.Nil(t, err)

	var parsed map[string]interface{}
	err = json.Unmarshal(data, &parsed)
	require.Nil(t, err)

	assert.Equal(t, "t-alpha", parsed["id"])
	assert.Equal(t, "alpha-org", parsed["org"])
	assert.Equal(t, "Alpha Corp", parsed["orgDisplayName"])

	instanceTypes := parsed["instanceTypes"].([]interface{})
	assert.Equal(t, 2, len(instanceTypes))

	gpuLEntry := instanceTypes[0].(map[string]interface{})
	assert.Equal(t, "gpu-large", gpuLEntry["name"])
	assert.Equal(t, float64(6), gpuLEntry["allocated"])
	assert.Equal(t, float64(2), gpuLEntry["maxAllocatable"])

	gpuLUsed := gpuLEntry["usedMachineStats"].(map[string]interface{})
	assert.Equal(t, float64(3), gpuLUsed["total"])
	assert.Equal(t, float64(2), gpuLUsed["inUse"])
	assert.Equal(t, float64(1), gpuLUsed["error"])

	gpuSEntry := instanceTypes[1].(map[string]interface{})
	assert.Equal(t, "gpu-small", gpuSEntry["name"])
	gpuSUsed := gpuSEntry["usedMachineStats"].(map[string]interface{})
	assert.Equal(t, float64(2), gpuSUsed["total"])
	assert.Equal(t, float64(2), gpuSUsed["inUse"])

	gpuLAllocs := gpuLEntry["allocations"].([]interface{})
	assert.Equal(t, 2, len(gpuLAllocs))
	assert.Equal(t, "training-reserved", gpuLAllocs[0].(map[string]interface{})["name"])
	assert.Equal(t, float64(4), gpuLAllocs[0].(map[string]interface{})["total"])

	var roundTrip APITenantInstanceTypeStats
	err = json.Unmarshal(data, &roundTrip)
	require.Nil(t, err)
	assert.Equal(t, stats, roundTrip)
}

func TestAPITenantInstanceTypeStats_EmptyInstanceTypes(t *testing.T) {
	stats := APITenantInstanceTypeStats{
		ID:             "tenant-empty",
		Org:            "empty-org",
		OrgDisplayName: "Empty Org",
		InstanceTypes:  nil,
	}

	data, err := json.Marshal(stats)
	require.Nil(t, err)

	var parsed map[string]interface{}
	err = json.Unmarshal(data, &parsed)
	require.Nil(t, err)

	assert.Equal(t, "tenant-empty", parsed["id"])
	assert.Nil(t, parsed["instanceTypes"])
}
