// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"
	"net/http"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
)

// Machine represents a simplified Machine
type Machine struct {
	ID                  string                  `json:"id"`
	InstanceTypeID      *string                 `json:"instanceTypeId"`
	InstanceID          *string                 `json:"instanceId"`
	Vendor              *string                 `json:"vendor"`
	ProductName         *string                 `json:"productName"`
	SerialNumber        *string                 `json:"serialNumber"`
	Hostname            *string                 `json:"hostname"`
	MachineCapabilities []MachineCapability     `json:"machineCapabilities"`
	AdminInterfaces     []MachineAdminInterface `json:"adminInterfaces"`
	MaintenanceMessage  *string                 `json:"maintenanceMessage"`
	ScoutVersion        *string                 `json:"scoutVersion"`
	Labels              map[string]string       `json:"labels"`
	Status              string                  `json:"status"`
	Created             time.Time               `json:"created"`
	Updated             time.Time               `json:"updated"`
}

// MachineCapability represents a machine capability
type MachineCapability struct {
	Type            string  `json:"type"`
	Name            string  `json:"name"`
	Frequency       *string `json:"frequency"`
	Cores           *int    `json:"cores"`
	Threads         *int    `json:"threads"`
	Capacity        *string `json:"capacity"`
	Vendor          *string `json:"vendor"`
	InactiveDevices []int   `json:"inactiveDevices"`
	Count           *int    `json:"count"`
	DeviceType      *string `json:"deviceType"`
}

// MachineAdminInterface describes an interface attached to a machine
type MachineAdminInterface struct {
	ID          *string    `json:"id"`
	IsPrimary   *bool      `json:"isPrimary"`
	MacAddress  *string    `json:"macAddress"`
	IpAddresses []string   `json:"ipAddresses"`
	Created     *time.Time `json:"created"`
	Updated     *time.Time `json:"updated"`
}

// MachineManager manages Machine operations
type MachineManager struct {
	client *Client
}

// NewMachineManager creates a new MachineManager
func NewMachineManager(client *Client) MachineManager {
	return MachineManager{client: client}
}

func machineFromStandard(api standard.Machine) Machine {
	m := Machine{
		Vendor:       api.Vendor.Get(),
		ProductName:  api.ProductName.Get(),
		SerialNumber: api.SerialNumber.Get(),
		Labels:       api.Labels,
	}
	if api.Id != nil {
		m.ID = *api.Id
	}
	if api.InstanceTypeId.IsSet() {
		m.InstanceTypeID = api.InstanceTypeId.Get()
	}
	if api.InstanceId.IsSet() {
		m.InstanceID = api.InstanceId.Get()
	}
	if api.MaintenanceMessage.IsSet() {
		m.MaintenanceMessage = api.MaintenanceMessage.Get()
	}
	if api.ScoutVersion.IsSet() {
		m.ScoutVersion = api.ScoutVersion.Get()
	}
	if api.Status != nil {
		m.Status = string(*api.Status)
	}
	if api.Created != nil {
		m.Created = *api.Created
	}
	if api.Updated != nil {
		m.Updated = *api.Updated
	}
	for _, mc := range api.MachineCapabilities {
		mmc := MachineCapability{}
		if mc.Name != nil {
			mmc.Name = *mc.Name
		}
		if mc.Type != nil {
			mmc.Type = *mc.Type
		}
		if mc.Frequency.IsSet() {
			mmc.Frequency = mc.Frequency.Get()
		}
		if mc.Cores.IsSet() && mc.Cores.Get() != nil {
			c := int(*mc.Cores.Get())
			mmc.Cores = &c
		}
		if mc.Threads.IsSet() && mc.Threads.Get() != nil {
			t := int(*mc.Threads.Get())
			mmc.Threads = &t
		}
		if mc.Capacity.IsSet() {
			mmc.Capacity = mc.Capacity.Get()
		}
		if mc.Vendor.IsSet() {
			mmc.Vendor = mc.Vendor.Get()
		}
		if mc.Count.IsSet() && mc.Count.Get() != nil {
			c := int(*mc.Count.Get())
			mmc.Count = &c
		}
		if mc.DeviceType.IsSet() {
			mmc.DeviceType = mc.DeviceType.Get()
		}
		if mc.InactiveDevices != nil {
			mmc.InactiveDevices = make([]int, len(mc.InactiveDevices))
			for i, d := range mc.InactiveDevices {
				mmc.InactiveDevices[i] = int(d)
			}
		}
		m.MachineCapabilities = append(m.MachineCapabilities, mmc)
	}
	for _, mi := range api.MachineInterfaces {
		mai := MachineAdminInterface{
			ID:          mi.Id,
			IsPrimary:   mi.IsPrimary,
			MacAddress:  mi.MacAddress.Get(),
			IpAddresses: mi.IpAddresses,
			Created:     mi.Created,
			Updated:     mi.Updated,
		}
		m.AdminInterfaces = append(m.AdminInterfaces, mai)
		if m.Hostname == nil && mi.Hostname.IsSet() && (mi.IsPrimary != nil && *mi.IsPrimary) {
			m.Hostname = mi.Hostname.Get()
		}
	}
	if m.Hostname == nil && len(api.MachineInterfaces) > 0 && api.MachineInterfaces[0].Hostname.IsSet() {
		m.Hostname = api.MachineInterfaces[0].Hostname.Get()
	}
	return m
}

// GetMachines returns all Machines
func (mm MachineManager) GetMachines(ctx context.Context, paginationFilter *PaginationFilter) ([]Machine, *standard.PaginationResponse, *ApiError) {
	ctx = WithLogger(ctx, mm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, mm.client.Config.Token)

	gmr := mm.client.apiClient.MachineAPI.GetAllMachine(ctx, mm.client.apiMetadata.Organization).
		SiteId(mm.client.apiMetadata.SiteID).IncludeMetadata(true)
	if paginationFilter != nil {
		if paginationFilter.PageNumber != nil {
			gmr = gmr.PageNumber(int32(*paginationFilter.PageNumber))
		}
		if paginationFilter.PageSize != nil {
			gmr = gmr.PageSize(int32(*paginationFilter.PageSize))
		}
		if paginationFilter.OrderBy != nil {
			gmr = gmr.OrderBy(*paginationFilter.OrderBy)
		}
	}

	apiMachines, resp, err := gmr.Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, nil, apiErr
	}

	machines := make([]Machine, 0, len(apiMachines))
	for _, apiMachine := range apiMachines {
		machines = append(machines, machineFromStandard(apiMachine))
	}

	paginationResponse, perr := standard.GetPaginationResponse(ctx, resp)
	if perr != nil {
		return nil, nil, &ApiError{
			Code:    http.StatusInternalServerError,
			Message: "failed to extract pagination: " + perr.Error(),
			Data:    map[string]interface{}{"parseError": perr.Error()},
		}
	}
	return machines, paginationResponse, nil
}

// GetMachine returns a Machine by ID
func (mm MachineManager) GetMachine(ctx context.Context, id string) (*Machine, *ApiError) {
	ctx = WithLogger(ctx, mm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, mm.client.Config.Token)

	apiMachine, resp, err := mm.client.apiClient.MachineAPI.GetMachine(ctx, mm.client.apiMetadata.Organization, id).
		IncludeMetadata(true).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	m := machineFromStandard(*apiMachine)
	return &m, nil
}

// GetDpuMachines returns the DPU machines attached to the host Machine by ID.
// TODO: returns the generated standard.DpuMachine; add a simplified type if callers need one.
func (mm MachineManager) GetDpuMachines(ctx context.Context, machineID string) ([]standard.DpuMachine, *ApiError) {
	ctx = WithLogger(ctx, mm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, mm.client.Config.Token)

	dpuMachines, resp, err := mm.client.apiClient.MachineAPI.
		GetDpuMachines(ctx, mm.client.apiMetadata.Organization, machineID).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	return dpuMachines, nil
}
