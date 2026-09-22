// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"net/http"
	"slices"
	"sort"

	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	dpuMachineOrderByFieldID = "id"
	dpuMachineOrderByIDAsc   = "ID_ASC"
	dpuMachineOrderByIDDesc  = "ID_DESC"
)

// GetAllDpuMachinesHandler lists DPU machines for a Site via Core.
type GetAllDpuMachinesHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// NewGetAllDpuMachinesHandler returns a handler for listing DPU machines.
func NewGetAllDpuMachinesHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetAllDpuMachinesHandler {
	return GetAllDpuMachinesHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Retrieve all DPU Machines for a Site
// @Description Retrieve DPU Machines for a Site, ordered by DPU Machine ID. Network configuration is null because the bulk Core Machine response does not include it.
// @Tags Machine
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of Site"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "DPU Machine ID ordering" Enums(ID_ASC, ID_DESC) default(ID_ASC)
// @Success 200 {array} model.APIDpuMachine
// @Router /v2/org/{org}/nico/dpu [get]
func (gadmh GetAllDpuMachinesHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("DpuMachine", "GetAll", c, gadmh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiRequest := model.APIGetAllDpuMachineRequest{}
	err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest, pagination.PageRequest{})
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = c.Bind(&apiRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	err = apiRequest.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating DPU Machine retrieval request data", err)
	}

	pageRequest := pagination.PageRequest{}
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}
	if pageRequest.OrderByStr == nil {
		pageRequest.OrderByStr = cutil.GetPtr(dpuMachineOrderByIDAsc)
	}
	err = pageRequest.Validate([]string{dpuMachineOrderByFieldID})
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	stc, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx:       ctx,
		Logger:    logger,
		DBSession: gadmh.dbSession,
		SCP:       gadmh.scp,
		Org:       org,
		User:      dbUser,
		SiteID:    apiRequest.SiteID,
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}
	site, err := common.GetSiteFromIDString(ctx, nil, siteID, gadmh.dbSession)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve authorized Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site due to DB error", nil)
	}

	var idList corev1.MachineIdList
	apiErr = common.ExecuteCoreGRPC(
		ctx,
		stc,
		corev1.Forge_FindMachineIds_FullMethodName,
		&corev1.MachineSearchConfig{IncludeDpus: true, ExcludeHosts: true},
		&idList,
		siteID,
	)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to retrieve DPU Machine IDs from Site")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	allIDs := make([]string, 0, len(idList.GetMachineIds()))
	for _, machineID := range idList.GetMachineIds() {
		if machineID.GetId() != "" {
			allIDs = append(allIDs, machineID.GetId())
		}
	}
	if *pageRequest.OrderByStr == dpuMachineOrderByIDDesc {
		sort.Sort(sort.Reverse(sort.StringSlice(allIDs)))
	} else {
		sort.Strings(allIDs)
	}
	total := len(allIDs)

	start := min(*pageRequest.Offset, total)
	end := min(start+*pageRequest.Limit, total)
	pageIDs := allIDs[start:end]

	apiDpuMachines := make([]model.APIDpuMachine, 0, len(pageIDs))
	if len(pageIDs) > 0 {
		machineIDs := make([]*corev1.MachineId, 0, len(pageIDs))
		for _, machineID := range pageIDs {
			machineIDs = append(machineIDs, &corev1.MachineId{Id: machineID})
		}
		var machineList corev1.MachineList
		apiErr = common.ExecuteCoreGRPC(
			ctx,
			stc,
			corev1.Forge_FindMachinesByIds_FullMethodName,
			&corev1.MachinesByIdsRequest{MachineIds: machineIDs},
			&machineList,
			siteID,
		)
		if apiErr != nil {
			logAPIError(logger, apiErr, "failed to retrieve DPU Machines by ID")
			return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
		}

		machinesByID := make(map[string]*corev1.Machine, len(machineList.GetMachines()))
		for _, machine := range machineList.GetMachines() {
			machinesByID[machine.GetId().GetId()] = machine
		}
		for _, machineID := range pageIDs {
			machine, ok := machinesByID[machineID]
			if !ok {
				continue
			}
			hostMachineID := machine.GetStatus().GetAssociatedHostMachineId().GetId()
			apiDpuMachine := model.APIDpuMachine{}
			apiDpuMachine.FromProto(&corev1.DpuMachine{Machine: machine}, model.APIDpuMachineProtoContext{
				HostMachineID:            hostMachineID,
				SiteID:                   site.ID,
				InfrastructureProviderID: site.InfrastructureProviderID,
			})
			apiDpuMachines = append(apiDpuMachines, apiDpuMachine)
		}
	}

	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().Int("total", total).Int("returned", len(apiDpuMachines)).Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiDpuMachines)
}

// GetDpuMachineHandler retrieves one DPU Machine for a Site via Core.
type GetDpuMachineHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// NewGetDpuMachineHandler returns a handler for retrieving one DPU Machine.
func NewGetDpuMachineHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetDpuMachineHandler {
	return GetDpuMachineHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Retrieve a DPU Machine for a Site
// @Description Retrieve one DPU Machine for a Site. Network configuration is null unless includeNetworkConfig is true.
// @Tags Machine
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param dpuMachineId path string true "ID of DPU Machine"
// @Param siteId query string true "ID of Site"
// @Param includeNetworkConfig query boolean false "Include DPU network configuration" default(false)
// @Success 200 {object} model.APIDpuMachine
// @Router /v2/org/{org}/nico/dpu/{dpuMachineId} [get]
func (gdmh GetDpuMachineHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("DpuMachine", "Get", c, gdmh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiRequest := model.APIGetDpuMachineRequest{}
	err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = c.Bind(&apiRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	err = apiRequest.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating DPU Machine retrieval request data", err)
	}

	dpuMachineID := c.Param("id")
	stc, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx:       ctx,
		Logger:    logger,
		DBSession: gdmh.dbSession,
		SCP:       gdmh.scp,
		Org:       org,
		User:      dbUser,
		SiteID:    apiRequest.SiteID,
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}
	site, err := common.GetSiteFromIDString(ctx, nil, siteID, gdmh.dbSession)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve authorized Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site due to DB error", nil)
	}

	var idList corev1.MachineIdList
	apiErr = common.ExecuteCoreGRPC(
		ctx,
		stc,
		corev1.Forge_FindMachineIds_FullMethodName,
		&corev1.MachineSearchConfig{IncludeDpus: true, ExcludeHosts: true},
		&idList,
		siteID,
	)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to retrieve DPU Machine IDs from Site")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	if !slices.ContainsFunc(idList.GetMachineIds(), func(machineID *corev1.MachineId) bool {
		return machineID.GetId() == dpuMachineID
	}) {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find DPU Machine with specified ID", nil)
	}

	var machineList corev1.MachineList
	apiErr = common.ExecuteCoreGRPC(
		ctx,
		stc,
		corev1.Forge_FindMachinesByIds_FullMethodName,
		&corev1.MachinesByIdsRequest{MachineIds: []*corev1.MachineId{{Id: dpuMachineID}}},
		&machineList,
		siteID,
	)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to retrieve DPU Machine by ID")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	var machine *corev1.Machine
	for _, candidate := range machineList.GetMachines() {
		if candidate.GetId().GetId() == dpuMachineID && candidate.GetMachineType() == corev1.MachineType_DPU {
			machine = candidate
			break
		}
	}
	if machine == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find DPU Machine with specified ID", nil)
	}

	dpuMachine := &corev1.DpuMachine{Machine: machine}
	if apiRequest.IncludeNetworkConfig {
		networkConfig := &corev1.ManagedHostNetworkConfigResponse{}
		apiErr = common.ExecuteCoreGRPC(
			ctx,
			stc,
			corev1.Forge_GetManagedHostNetworkConfig_FullMethodName,
			&corev1.ManagedHostNetworkConfigRequest{DpuMachineId: &corev1.MachineId{Id: dpuMachineID}},
			networkConfig,
			siteID,
		)
		if apiErr != nil {
			logAPIError(logger, apiErr, "failed to retrieve DPU Machine network configuration")
			return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
		}
		dpuMachine.DpuNetworkConfig = networkConfig
	}

	apiDpuMachine := model.APIDpuMachine{}
	apiDpuMachine.FromProto(dpuMachine, model.APIDpuMachineProtoContext{
		HostMachineID:            machine.GetStatus().GetAssociatedHostMachineId().GetId(),
		SiteID:                   site.ID,
		InfrastructureProviderID: site.InfrastructureProviderID,
	})

	logger.Info().Str("dpuMachineId", dpuMachineID).Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiDpuMachine)
}
