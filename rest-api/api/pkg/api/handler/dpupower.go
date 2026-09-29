// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"errors"
	"net/http"
	"slices"

	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// DpuPowerControlHandler performs the guarded, site-scoped DPU restart used by
// break-fix automation.
type DpuPowerControlHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

func NewDpuPowerControlHandler(dbSession *cdb.Session, scp *sc.ClientPool) DpuPowerControlHandler {
	return DpuPowerControlHandler{dbSession: dbSession, scp: scp, tracerSpan: cutil.NewTracerSpan()}
}

// Handle godoc
// @Summary Gracefully restart a DPU Machine
// @Description Accept a site-scoped GracefulRestart for a DPU Machine. HTTP 202 means accepted, not completed; callers must observe the DPU returning to service. Because an ambiguous failure may occur after dispatch, callers must not retry automatically.
// @Tags DPU Machine
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param dpuMachineId path string true "ID of DPU Machine"
// @Param siteId query string true "ID of Site"
// @Param request body model.APIDpuPowerControlRequest true "DPU power control request"
// @Success 202 {object} model.APIMessageResponse
// @Router /v2/org/{org}/nico/dpu/{dpuMachineId}/power [patch]
func (h DpuPowerControlHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, span := common.SetupHandler("DpuMachine", "PowerControl", c, h.tracerSpan)
	if span != nil {
		defer span.End()
	}

	query := model.APIDpuPowerControlQuery{}
	if err := common.ValidateKnownQueryParams(c.QueryParams(), query); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	query.SiteID = c.QueryParam("siteId")
	if err := query.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating DPU power control query", err)
	}

	request := model.APIDpuPowerControlRequest{}
	if err := c.Bind(&request); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	if err := request.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating DPU power control request data", err)
	}

	dpuMachineID := c.Param("id")
	if dpuMachineID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "DPU Machine ID was not specified in URL", nil)
	}

	stc, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx: ctx, Logger: logger, DBSession: h.dbSession, SCP: h.scp, Org: org, User: dbUser, SiteID: query.SiteID,
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	var ids corev1.MachineIdList
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_FindMachineIds_FullMethodName,
		&corev1.MachineSearchConfig{IncludeDpus: true, ExcludeHosts: true}, &ids, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to find DPU Machines")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	if !slices.ContainsFunc(ids.GetMachineIds(), func(id *corev1.MachineId) bool { return id.GetId() == dpuMachineID }) {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find DPU Machine with specified ID", nil)
	}

	var machines corev1.MachineList
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_FindMachinesByIds_FullMethodName,
		&corev1.MachinesByIdsRequest{MachineIds: []*corev1.MachineId{{Id: dpuMachineID}}}, &machines, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to retrieve DPU Machine")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	var dpu *corev1.Machine
	for _, candidate := range machines.GetMachines() {
		if candidate.GetId().GetId() == dpuMachineID && candidate.GetMachineType() == corev1.MachineType_DPU {
			dpu = candidate
			break
		}
	}
	if dpu == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find DPU Machine with specified ID", nil)
	}

	hostMachineID := dpu.GetStatus().GetAssociatedHostMachineId().GetId()
	if hostMachineID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "DPU Machine is not associated with a host Machine", nil)
	}
	host, err := cdbm.NewMachineDAO(h.dbSession).GetByID(ctx, nil, hostMachineID, nil, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "DPU Machine's associated host Machine was not found", nil)
		}
		logger.Error().Err(err).Str("host_machine_id", hostMachineID).Msg("failed to retrieve associated host Machine")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve associated host Machine", nil)
	}
	if host.SiteID.String() != siteID {
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "DPU Machine and associated host Machine are not in the same Site", nil)
	}
	if host.IsAssigned && (request.AcknowledgeAttachedInstance == nil || !*request.AcknowledgeAttachedInstance) {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "DPU Machine's host is currently in use by an Instance, set acknowledgeAttachedInstance to true to proceed", nil)
	}
	if !host.IsAssigned && request.AcknowledgeAttachedInstance != nil && *request.AcknowledgeAttachedInstance {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "DPU Machine's host has no attached Instance to acknowledge", nil)
	}

	logger.Info().Str("site_id", siteID).Str("dpu_machine_id", dpuMachineID).Str("host_machine_id", hostMachineID).
		Str("action", string(request.Action)).Msg("accepting DPU power control request")
	response := &corev1.AdminPowerControlResponse{}
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_AdminPowerControl_FullMethodName, request.ToProto(dpuMachineID), response, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to execute DPU power control request")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	logger.Info().Str("site_id", siteID).Str("dpu_machine_id", dpuMachineID).Msg("DPU power control request accepted")
	return c.JSON(http.StatusAccepted, model.APIMessageResponse{Message: response.GetMsg()})
}
