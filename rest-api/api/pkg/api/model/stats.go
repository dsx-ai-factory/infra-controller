// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"github.com/google/uuid"
	"github.com/samber/lo"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

// ~~~~~ Machine GPU Stats ~~~~~ //

// APIMachineGPUStats represents GPU summary stats for a single GPU type across machines at a site
type APIMachineGPUStats struct {
	// Name is the GPU name from the MachineCapability record
	Name string `json:"name"`
	// GPUs is the total number of GPUs (summation of all Machine GPU capability counts)
	GPUs int `json:"gpus"`
	// Machines is the number of machines that have this GPU capability
	Machines int `json:"machines"`
}

// ~~~~~ Tenant Instance Type Stats ~~~~~ //

// APITenantInstanceTypeStats represents per-tenant instance type allocation stats
type APITenantInstanceTypeStats struct {
	// ID is the unique identifier for the Tenant
	ID string `json:"id"`
	// Org is the organization name for the Tenant
	Org string `json:"org"`
	// OrgDisplayName is the display name for the Tenant's organization
	OrgDisplayName string `json:"orgDisplayName"`
	// InstanceTypes is the list of instance type stats for this tenant
	InstanceTypes []APITenantInstanceTypeStatsEntry `json:"instanceTypes"`
}

// APITenantInstanceTypeStatsEntry represents stats for a single instance type within a tenant
type APITenantInstanceTypeStatsEntry struct {
	// ID is the unique identifier for the InstanceType
	ID string `json:"id"`
	// Name is the name of the InstanceType
	Name string `json:"name"`
	// Allocated is the number of Machines of this Instance Type allocated to this Tenant
	Allocated int `json:"allocated"`
	// UsedMachineStats captures the usage status of machines for this instance type within the tenant
	UsedMachineStats APIMachineStatusBreakdown `json:"usedMachineStats"`
	// MaxAllocatable is the number of Ready Machines of this Instance Type available for additional allocation to Tenants
	MaxAllocatable int `json:"maxAllocatable"`
	// Allocations is the list of individual allocations for this instance type within the tenant
	Allocations []APITenantInstanceTypeAllocationStats `json:"allocations"`
}

// APITenantInstanceTypeAllocation represents a single allocation's stats for an instance type
type APITenantInstanceTypeAllocationStats struct {
	// ID is the unique identifier for the Allocation
	ID string `json:"id"`
	// Name is the name of the Allocation
	Name string `json:"name"`
	// Total is the total number of machines in this allocation for the instance type
	Total int `json:"total"`
}

// ~~~~~ Machine Instance Type Summary ~~~~~ //

// APIMachineInstanceTypeSummary represents a summary of machines grouped by assigned vs unassigned
type APIMachineInstanceTypeSummary struct {
	// Assigned represents machines that have been assigned to an instance type
	Assigned APIMachineStatusBreakdown `json:"assigned"`
	// Unassigned represents machines that have not been assigned to any instance type
	Unassigned APIMachineStatusBreakdown `json:"unassigned"`
}

// APIMachineStatusBreakdown represents machine counts broken down by status
type APIMachineStatusBreakdown struct {
	// Total is the total number of machines in this group
	Total int `json:"total"`
	// Initializing is the number of machines being initialized
	Initializing int `json:"initializing"`
	// Ready is the number of machines in ready state
	Ready int `json:"ready"`
	// InUse is the number of machines currently in use
	InUse int `json:"inUse"`
	// Error is the number of machines in error state
	Error int `json:"error"`
	// Maintenance is the number of machines in maintenance state
	Maintenance int `json:"maintenance"`
	// Unknown is the number of machines in unknown state
	Unknown int `json:"unknown"`
}

// AddMachineStatusCounts increments counters based on machine status.
func (amsb *APIMachineStatusBreakdown) AddMachineStatusCounts(m cdbm.Machine) {
	amsb.Total++
	switch m.Status {
	case cdbm.MachineStatusInitializing:
		amsb.Initializing++
	case cdbm.MachineStatusReady:
		amsb.Ready++
	case cdbm.MachineStatusInUse:
		amsb.InUse++
	case cdbm.MachineStatusError:
		amsb.Error++
	case cdbm.MachineStatusMaintenance:
		amsb.Maintenance++
	case cdbm.MachineStatusUnknown:
		amsb.Unknown++
	}
}

// ~~~~~ Machine Instance Type Detailed Stats ~~~~~ //

// APIMachineInstanceTypeStats represents detailed stats for machines of a specific instance type
type APIMachineInstanceTypeStats struct {
	// ID is the unique identifier for the InstanceType
	ID string `json:"id"`
	// Name is the name of the InstanceType
	Name string `json:"name"`
	// AssignedMachineStats captures the status of all Machines assigned to this Instance Type
	AssignedMachineStats APIMachineStatusBreakdown `json:"assignedMachineStats"`
	// Allocated is the number of Machines of this Instance Type allocated to Tenants
	Allocated int `json:"allocated"`
	// MaxAllocatable is the number of Ready Machines of this Instance Type available for additional allocation to Tenants
	MaxAllocatable int `json:"maxAllocatable"`
	// UsedMachineStats captures the usage status of machines assigned to this instance type
	// that are currently associated with Tenant Instances
	UsedMachineStats APIMachineStatusBreakdown `json:"usedMachineStats"`
	// Tenants is the per-tenant breakdown for this instance type
	Tenants []APIMachineInstanceTypeTenantStats `json:"tenants"`
}

// APIMachineInstanceTypeTenant represents per-tenant allocation stats within an instance type
type APIMachineInstanceTypeTenantStats struct {
	// ID is the unique identifier for the Tenant
	ID string `json:"id"`
	// Name is the name of the Tenant
	Name string `json:"name"`
	// Allocated is the number of Machines allocated to this Tenant for this Instance Type
	Allocated int `json:"allocated"`
	// UsedMachineStats captures the usage status of machines for this tenant and instance type
	UsedMachineStats APIMachineStatusBreakdown `json:"usedMachineStats"`
	// Allocations is the list of individual allocations for this tenant and instance type
	Allocations []APIMachineInstanceTypeTenantAllocationStats `json:"allocations"`
}

// APIMachineInstanceTypeTenantAllocation represents a single allocation within a tenant's instance type stats
type APIMachineInstanceTypeTenantAllocationStats struct {
	// ID is the unique identifier for the Allocation
	ID string `json:"id"`
	// Name is the name of the Allocation
	Name string `json:"name"`
	// Allocated is the total number of machines in this allocation for the instance type
	Allocated int `json:"allocated"`
}

// NewAPIMachineInstanceTypeStats builds a single APIMachineInstanceTypeStats for one instance type
func NewAPIMachineInstanceTypeStats(
	it cdbm.InstanceType,
	itMachines []cdbm.Machine,
	itConstraints []cdbm.AllocationConstraint,
	itUsed map[uuid.UUID]*APIMachineStatusBreakdown,
	tenantITUsed map[uuid.UUID]map[uuid.UUID]*APIMachineStatusBreakdown,
) APIMachineInstanceTypeStats {
	assignedStats := &APIMachineStatusBreakdown{}
	for _, m := range itMachines {
		assignedStats.AddMachineStatusCounts(m)
	}

	allocated := lo.Reduce(itConstraints, func(acc int, ac cdbm.AllocationConstraint, _ int) int {
		return acc + ac.ConstraintValue
	}, 0)

	used := APIMachineStatusBreakdown{}
	if itUsed[it.ID] != nil {
		used = *itUsed[it.ID]
	}

	// ready machines minus those already reserved (allocated but not yet in use)
	maxAlloc := max(0, assignedStats.Ready-(allocated-used.Total))

	tenantMap := make(map[uuid.UUID]*APIMachineInstanceTypeTenantStats)
	for _, ac := range itConstraints {
		tID := ac.Allocation.TenantID
		tenantEntry, exists := tenantMap[tID]
		if !exists {
			tenantEntry = &APIMachineInstanceTypeTenantStats{
				ID:   tID.String(),
				Name: ac.Allocation.Tenant.Org,
			}
			tenantMap[tID] = tenantEntry
		}
		tenantEntry.Allocated += ac.ConstraintValue
		tenantEntry.Allocations = append(tenantEntry.Allocations, APIMachineInstanceTypeTenantAllocationStats{
			ID:        ac.Allocation.ID.String(),
			Name:      ac.Allocation.Name,
			Allocated: ac.ConstraintValue,
		})
	}

	for tID, tenantEntry := range tenantMap {
		if tenantITUsed[tID] != nil && tenantITUsed[tID][it.ID] != nil {
			tenantEntry.UsedMachineStats = *tenantITUsed[tID][it.ID]
		}
	}

	tenants := lo.MapToSlice(tenantMap, func(_ uuid.UUID, t *APIMachineInstanceTypeTenantStats) APIMachineInstanceTypeTenantStats {
		return *t
	})

	return APIMachineInstanceTypeStats{
		ID:                   it.ID.String(),
		Name:                 it.Name,
		AssignedMachineStats: *assignedStats,
		Allocated:            allocated,
		MaxAllocatable:       maxAlloc,
		UsedMachineStats:     used,
		Tenants:              tenants,
	}
}

// GetInstanceTypeMachineUsageMap builds per-instance-type and per-tenant-instance-type usage maps from instances
func GetInstanceTypeMachineUsageMap(instances []cdbm.Instance, machineByID map[string]cdbm.Machine) (
	itUsed map[uuid.UUID]*APIMachineStatusBreakdown,
	tenantITUsed map[uuid.UUID]map[uuid.UUID]*APIMachineStatusBreakdown,
) {
	itUsed = make(map[uuid.UUID]*APIMachineStatusBreakdown)
	tenantITUsed = make(map[uuid.UUID]map[uuid.UUID]*APIMachineStatusBreakdown)

	for _, inst := range instances {
		if inst.InstanceTypeID == nil || inst.MachineID == nil {
			continue
		}
		itID := *inst.InstanceTypeID
		tID := inst.TenantID

		if itUsed[itID] == nil {
			itUsed[itID] = &APIMachineStatusBreakdown{}
		}
		if tenantITUsed[tID] == nil {
			tenantITUsed[tID] = make(map[uuid.UUID]*APIMachineStatusBreakdown)
		}
		if tenantITUsed[tID][itID] == nil {
			tenantITUsed[tID][itID] = &APIMachineStatusBreakdown{}
		}

		if m, ok := machineByID[*inst.MachineID]; ok {
			itUsed[itID].AddMachineStatusCounts(m)
			tenantITUsed[tID][itID].AddMachineStatusCounts(m)
		}
	}

	return itUsed, tenantITUsed
}
