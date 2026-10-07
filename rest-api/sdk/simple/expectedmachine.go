// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"
	"fmt"
	"net/http"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
)

// ExpectedMachine represents a simplified Expected Machine
type ExpectedMachine struct {
	ID                       string                     `json:"id"`
	BmcMacAddress            string                     `json:"bmcMacAddress"`
	ChassisSerialNumber      string                     `json:"chassisSerialNumber"`
	FallbackDPUSerialNumbers []string                   `json:"fallbackDPUSerialNumbers"`
	Interfaces               []ExpectedMachineInterface `json:"interfaces"`
	SkuID                    *string                    `json:"skuId"`
	Sku                      *standard.Sku              `json:"sku,omitempty"`
	MachineID                *string                    `json:"machineId"`
	Machine                  *standard.MachineSummary   `json:"machine,omitempty"`
	Labels                   map[string]string          `json:"labels"`
	Created                  time.Time                  `json:"created"`
	Updated                  time.Time                  `json:"updated"`
}

// ExpectedMachineInterface is expected host NIC information forwarded to NICo Core.
type ExpectedMachineInterface struct {
	MacAddress         string  `json:"macAddress"`
	NicType            *string `json:"nicType"`
	FixedIP            *string `json:"fixedIp"`
	FixedMask          *string `json:"fixedMask"`
	FixedGateway       *string `json:"fixedGateway"`
	Primary            *bool   `json:"primary"`
	NetworkSegmentType *string `json:"networkSegmentType"`
	Role               *string `json:"role"`
	IPAllocation       *string `json:"ipAllocation"`
}

// ExpectedMachineCreateRequest represents a request to create an Expected Machine
type ExpectedMachineCreateRequest struct {
	BmcMacAddress            string                     `json:"bmcMacAddress"`
	BmcUsername              *string                    `json:"bmcUsername"`
	BmcPassword              *string                    `json:"bmcPassword"`
	ChassisSerialNumber      string                     `json:"chassisSerialNumber"`
	FallbackDPUSerialNumbers []string                   `json:"fallbackDPUSerialNumbers"`
	Interfaces               []ExpectedMachineInterface `json:"interfaces"`
	Labels                   map[string]string          `json:"labels"`
}

// ExpectedMachineUpdateRequest represents a request to update an Expected Machine
type ExpectedMachineUpdateRequest struct {
	ID string `json:"id,omitempty"` // Required for batch operations
	// Deprecated: BmcMacAddress may reassert the current BMC MAC but cannot
	// change it.
	BmcMacAddress            *string                    `json:"bmcMacAddress"`
	BmcUsername              *string                    `json:"bmcUsername"`
	BmcPassword              *string                    `json:"bmcPassword"`
	ChassisSerialNumber      *string                    `json:"chassisSerialNumber"`
	FallbackDPUSerialNumbers []string                   `json:"fallbackDPUSerialNumbers"`
	Interfaces               []ExpectedMachineInterface `json:"interfaces"`
	SkuID                    *string                    `json:"skuId"`
	Labels                   map[string]string          `json:"labels"`
}

func expectedMachineInterfacesFromStandard(interfaces []standard.ExpectedMachineInterface) []ExpectedMachineInterface {
	result := make([]ExpectedMachineInterface, 0, len(interfaces))
	for _, expectedInterface := range interfaces {
		result = append(result, ExpectedMachineInterface{
			MacAddress:         expectedInterface.MacAddress,
			NicType:            expectedInterface.NicType.Get(),
			FixedIP:            expectedInterface.FixedIp.Get(),
			FixedMask:          expectedInterface.FixedMask.Get(),
			FixedGateway:       expectedInterface.FixedGateway.Get(),
			Primary:            expectedInterface.Primary.Get(),
			NetworkSegmentType: expectedInterface.NetworkSegmentType.Get(),
			Role:               expectedInterface.Role.Get(),
			IPAllocation:       expectedInterface.IpAllocation.Get(),
		})
	}
	return result
}

func expectedMachineInterfacesToStandard(interfaces []ExpectedMachineInterface) []standard.ExpectedMachineInterface {
	if interfaces == nil {
		return nil
	}
	result := make([]standard.ExpectedMachineInterface, 0, len(interfaces))
	for _, expectedInterface := range interfaces {
		converted := standard.ExpectedMachineInterface{MacAddress: expectedInterface.MacAddress}
		if expectedInterface.NicType != nil {
			converted.NicType.Set(expectedInterface.NicType)
		}
		if expectedInterface.FixedIP != nil {
			converted.FixedIp.Set(expectedInterface.FixedIP)
		}
		if expectedInterface.FixedMask != nil {
			converted.FixedMask.Set(expectedInterface.FixedMask)
		}
		if expectedInterface.FixedGateway != nil {
			converted.FixedGateway.Set(expectedInterface.FixedGateway)
		}
		if expectedInterface.Primary != nil {
			converted.Primary.Set(expectedInterface.Primary)
		}
		if expectedInterface.NetworkSegmentType != nil {
			converted.NetworkSegmentType.Set(expectedInterface.NetworkSegmentType)
		}
		if expectedInterface.Role != nil {
			converted.Role.Set(expectedInterface.Role)
		}
		if expectedInterface.IPAllocation != nil {
			converted.IpAllocation.Set(expectedInterface.IPAllocation)
		}
		result = append(result, converted)
	}
	return result
}

// ExpectedMachineManager manages Expected Machine operations
type ExpectedMachineManager struct {
	client *Client
}

// NewExpectedMachineManager creates a new ExpectedMachineManager
func NewExpectedMachineManager(client *Client) ExpectedMachineManager {
	return ExpectedMachineManager{client: client}
}

func expectedMachineFromStandard(api standard.ExpectedMachine) ExpectedMachine {
	em := ExpectedMachine{
		FallbackDPUSerialNumbers: api.FallbackDPUSerialNumbers,
		Interfaces:               expectedMachineInterfacesFromStandard(api.Interfaces),
		Labels:                   api.Labels,
		Sku:                      api.Sku,
		Machine:                  api.Machine,
	}
	if api.Id != nil {
		em.ID = *api.Id
	}
	if api.BmcMacAddress != nil {
		em.BmcMacAddress = *api.BmcMacAddress
	}
	if api.ChassisSerialNumber != nil {
		em.ChassisSerialNumber = *api.ChassisSerialNumber
	}
	if api.SkuId.IsSet() {
		em.SkuID = api.SkuId.Get()
	}
	if api.MachineId.IsSet() {
		em.MachineID = api.MachineId.Get()
	}
	if api.Created != nil {
		em.Created = *api.Created
	}
	if api.Updated != nil {
		em.Updated = *api.Updated
	}
	return em
}

func toStandardExpectedMachineCreateRequest(request ExpectedMachineCreateRequest, siteID string) standard.ExpectedMachineCreateRequest {
	apiReq := standard.ExpectedMachineCreateRequest{
		SiteId:                   siteID,
		BmcMacAddress:            request.BmcMacAddress,
		ChassisSerialNumber:      request.ChassisSerialNumber,
		FallbackDPUSerialNumbers: request.FallbackDPUSerialNumbers,
		Interfaces:               expectedMachineInterfacesToStandard(request.Interfaces),
		Labels:                   request.Labels,
	}
	if request.BmcUsername != nil {
		apiReq.DefaultBmcUsername.Set(request.BmcUsername)
	}
	if request.BmcPassword != nil {
		apiReq.DefaultBmcPassword.Set(request.BmcPassword)
	}
	return apiReq
}

func toStandardExpectedMachineUpdateRequest(request ExpectedMachineUpdateRequest) standard.ExpectedMachineUpdateRequest {
	apiReq := standard.ExpectedMachineUpdateRequest{
		FallbackDPUSerialNumbers: request.FallbackDPUSerialNumbers,
		Interfaces:               expectedMachineInterfacesToStandard(request.Interfaces),
		Labels:                   request.Labels,
	}
	if request.ID != "" {
		apiReq.Id.Set(&request.ID)
	}
	if request.BmcMacAddress != nil {
		apiReq.BmcMacAddress.Set(request.BmcMacAddress)
	}
	if request.BmcUsername != nil {
		apiReq.DefaultBmcUsername.Set(request.BmcUsername)
	}
	if request.BmcPassword != nil {
		apiReq.DefaultBmcPassword.Set(request.BmcPassword)
	}
	if request.ChassisSerialNumber != nil {
		apiReq.ChassisSerialNumber.Set(request.ChassisSerialNumber)
	}
	if request.SkuID != nil {
		apiReq.SkuId.Set(request.SkuID)
	}
	return apiReq
}

// Create creates a new Expected Machine
func (emm ExpectedMachineManager) Create(ctx context.Context, request ExpectedMachineCreateRequest) (*ExpectedMachine, *ApiError) {
	ctx = WithLogger(ctx, emm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, emm.client.Config.Token)

	apiReq := toStandardExpectedMachineCreateRequest(request, emm.client.apiMetadata.SiteID)
	apiEM, resp, err := emm.client.apiClient.ExpectedMachineAPI.CreateExpectedMachine(ctx, emm.client.apiMetadata.Organization).
		ExpectedMachineCreateRequest(apiReq).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	em := expectedMachineFromStandard(*apiEM)
	return &em, nil
}

// Get returns an Expected Machine by ID
func (emm ExpectedMachineManager) Get(ctx context.Context, id string) (*ExpectedMachine, *ApiError) {
	ctx = WithLogger(ctx, emm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, emm.client.Config.Token)

	apiEM, resp, err := emm.client.apiClient.ExpectedMachineAPI.GetExpectedMachine(ctx, emm.client.apiMetadata.Organization, id).
		IncludeRelation("Sku").IncludeRelation("Machine").Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	em := expectedMachineFromStandard(*apiEM)
	return &em, nil
}

// GetExpectedMachines returns all Expected Machines
func (emm ExpectedMachineManager) GetExpectedMachines(ctx context.Context, paginationFilter *PaginationFilter) ([]ExpectedMachine, *standard.PaginationResponse, *ApiError) {
	ctx = WithLogger(ctx, emm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, emm.client.Config.Token)

	gmr := emm.client.apiClient.ExpectedMachineAPI.GetAllExpectedMachine(ctx, emm.client.apiMetadata.Organization).
		SiteId(emm.client.apiMetadata.SiteID).IncludeRelation("Sku").IncludeRelation("Machine")
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

	apiEMs, resp, err := gmr.Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, nil, apiErr
	}

	ems := make([]ExpectedMachine, 0, len(apiEMs))
	for _, em := range apiEMs {
		ems = append(ems, expectedMachineFromStandard(em))
	}

	paginationResponse, perr := standard.GetPaginationResponse(ctx, resp)
	if perr != nil {
		return nil, nil, &ApiError{
			Code:    http.StatusInternalServerError,
			Message: "failed to extract pagination: " + perr.Error(),
			Data:    map[string]interface{}{"parseError": perr.Error()},
		}
	}
	return ems, paginationResponse, nil
}

// Update updates an Expected Machine
func (emm ExpectedMachineManager) Update(ctx context.Context, id string, request ExpectedMachineUpdateRequest) (*ExpectedMachine, *ApiError) {
	ctx = WithLogger(ctx, emm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, emm.client.Config.Token)

	apiReq := toStandardExpectedMachineUpdateRequest(request)
	apiEM, resp, err := emm.client.apiClient.ExpectedMachineAPI.UpdateExpectedMachine(ctx, emm.client.apiMetadata.Organization, id).
		ExpectedMachineUpdateRequest(apiReq).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	em := expectedMachineFromStandard(*apiEM)
	return &em, nil
}

// Delete deletes an Expected Machine
func (emm ExpectedMachineManager) Delete(ctx context.Context, id string) *ApiError {
	ctx = WithLogger(ctx, emm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, emm.client.Config.Token)

	resp, err := emm.client.apiClient.ExpectedMachineAPI.DeleteExpectedMachine(ctx, emm.client.apiMetadata.Organization, id).Execute()
	return HandleResponseError(resp, err)
}

// batchExpectedMachineLimit is the OpenAPI maxItems constraint for batch ExpectedMachine operations
const batchExpectedMachineLimit = 100

// BatchCreate creates multiple Expected Machines (max 100 per request)
func (emm ExpectedMachineManager) BatchCreate(ctx context.Context, requests []ExpectedMachineCreateRequest) ([]ExpectedMachine, *ApiError) {
	ctx = WithLogger(ctx, emm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, emm.client.Config.Token)

	if len(requests) == 0 {
		return nil, &ApiError{Code: http.StatusBadRequest, Message: "batch create requires at least 1 ExpectedMachine (minItems: 1)"}
	}
	if len(requests) > batchExpectedMachineLimit {
		return nil, &ApiError{Code: http.StatusBadRequest, Message: fmt.Sprintf("batch create exceeds maximum of %d ExpectedMachines per request (maxItems: %d)", batchExpectedMachineLimit, batchExpectedMachineLimit)}
	}

	apiReqs := make([]standard.ExpectedMachineCreateRequest, 0, len(requests))
	for _, request := range requests {
		apiReqs = append(apiReqs, toStandardExpectedMachineCreateRequest(request, emm.client.apiMetadata.SiteID))
	}
	apiEMs, resp, err := emm.client.apiClient.ExpectedMachineAPI.BatchCreateExpectedMachines(ctx, emm.client.apiMetadata.Organization).
		ExpectedMachineCreateRequest(apiReqs).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	ems := make([]ExpectedMachine, 0, len(apiEMs))
	for _, em := range apiEMs {
		ems = append(ems, expectedMachineFromStandard(em))
	}
	return ems, nil
}

// BatchUpdate updates multiple Expected Machines (max 100 per request). Each request must have ID set.
func (emm ExpectedMachineManager) BatchUpdate(ctx context.Context, updates []ExpectedMachineUpdateRequest) ([]ExpectedMachine, *ApiError) {
	ctx = WithLogger(ctx, emm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, emm.client.Config.Token)

	if len(updates) == 0 {
		return nil, &ApiError{Code: http.StatusBadRequest, Message: "batch update requires at least 1 ExpectedMachine (minItems: 1)"}
	}
	if len(updates) > batchExpectedMachineLimit {
		return nil, &ApiError{Code: http.StatusBadRequest, Message: fmt.Sprintf("batch update exceeds maximum of %d ExpectedMachines per request (maxItems: %d)", batchExpectedMachineLimit, batchExpectedMachineLimit)}
	}

	for i, u := range updates {
		if u.ID == "" {
			return nil, &ApiError{Code: http.StatusBadRequest, Message: fmt.Sprintf("update at index %d is missing required ID field", i)}
		}
	}
	apiReqs := make([]standard.ExpectedMachineUpdateRequest, 0, len(updates))
	for _, u := range updates {
		apiReqs = append(apiReqs, toStandardExpectedMachineUpdateRequest(u))
	}
	apiEMs, resp, err := emm.client.apiClient.ExpectedMachineAPI.BatchUpdateExpectedMachines(ctx, emm.client.apiMetadata.Organization).
		ExpectedMachineUpdateRequest(apiReqs).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	ems := make([]ExpectedMachine, 0, len(apiEMs))
	for _, em := range apiEMs {
		ems = append(ems, expectedMachineFromStandard(em))
	}
	return ems, nil
}
