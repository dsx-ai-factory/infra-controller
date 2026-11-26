// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"errors"
	"net/http"

	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// UpdateDpuMachinePowerHandler power controls one DPU Machine for a Site via Core.
type UpdateDpuMachinePowerHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewUpdateDpuMachinePowerHandler returns a handler for DPU Machine power control.
func NewUpdateDpuMachinePowerHandler(dbSession *cdb.Session, scp *sc.ClientPool) UpdateDpuMachinePowerHandler {
	return UpdateDpuMachinePowerHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle godoc
// @Summary DPU Machine power control
// @Description Execute a power control action on a DPU Machine. When an Instance is attached to the DPU's host Machine, or the host Machine's state is unknown, acknowledgeAttachedInstance must be true.
// @Tags DPU Machine
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param dpuMachineId path string true "ID of DPU Machine"
// @Param request body model.APIDpuMachinePowerUpdateRequest true "DPU Machine power control request"
// @Success 202 {object} model.APIMessageResponse
// @Router /v2/org/{org}/nico/dpu/{dpuMachineId}/power [patch]
func (udmph UpdateDpuMachinePowerHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("DpuMachinePower", "Update", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiRequest := model.APIDpuMachinePowerUpdateRequest{}
	err := c.Bind(&apiRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	err = apiRequest.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating DPU Machine power update request data", err)
	}
	apiRequest.DpuMachineID = c.Param("id")

	// Provider Admin only: resolveDpuMachine rejects Provider Viewer and Tenant roles.
	resolved, apiErr := resolveDpuMachine(resolveDpuMachineInput{
		Ctx:          ctx,
		Logger:       logger,
		DBSession:    udmph.dbSession,
		SCP:          udmph.scp,
		Org:          org,
		User:         dbUser,
		SiteID:       apiRequest.SiteID,
		DpuMachineID: apiRequest.DpuMachineID,
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}
	siteID := resolved.site.ID.String()

	// Power cycling a DPU disrupts the network of its host Machine, so an
	// Instance on that host must be acknowledged. Core names the host; the REST
	// Machine table says whether it is assigned. A DPU without a host in Core
	// cannot carry an Instance. A host that REST does not know, or that REST
	// records under another Site, has unknown assignment and is treated as if
	// an Instance were attached.
	hostMachineID := resolved.machine.GetStatus().GetAssociatedHostMachineId().GetId()
	isAcknowledged := apiRequest.AcknowledgeAttachedInstance != nil && *apiRequest.AcknowledgeAttachedInstance
	if hostMachineID != "" && !isAcknowledged {
		hostMachine, err := cdbm.NewMachineDAO(udmph.dbSession).GetByID(ctx, nil, hostMachineID, nil, false)
		if err != nil && !errors.Is(err, cdb.ErrDoesNotExist) {
			logger.Error().Err(err).Str("hostMachineId", hostMachineID).Msg("failed to retrieve host Machine of DPU Machine from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve host Machine of DPU Machine, DB error", nil)
		}
		if err != nil || hostMachine.SiteID != resolved.site.ID {
			logger.Warn().Str("hostMachineId", hostMachineID).Msg("host Machine of DPU Machine is not known for Site, power control requires acknowledgement")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not determine whether the DPU Machine's host Machine is in use by an Instance, set acknowledgeAttachedInstance to true to proceed", nil)
		}
		if hostMachine.IsAssigned {
			logger.Warn().Str("hostMachineId", hostMachineID).Msg("host Machine of DPU Machine is in use by an Instance, power control requires acknowledgement")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "DPU Machine's host Machine is currently in use by an Instance, set acknowledgeAttachedInstance to true to proceed", nil)
		}
	}

	logger.Info().Str("dpuMachineId", apiRequest.DpuMachineID).Str("hostMachineId", hostMachineID).Str("action", string(apiRequest.Action)).Str("siteId", siteID).Msg("sending DPU Machine power control request via Core gRPC proxy")

	coreResponse := &corev1.AdminPowerControlResponse{}
	apiErr = common.ExecuteCoreGRPC(ctx, resolved.stc, corev1.Forge_AdminPowerControl_FullMethodName, apiRequest.ToProto(), coreResponse, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to execute DPU Machine power control request via Core gRPC proxy")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	logger.Info().Str("dpuMachineId", apiRequest.DpuMachineID).Msg("finishing API handler")
	return c.JSON(http.StatusAccepted, model.APIMessageResponse{
		Message: coreResponse.GetMsg(),
	})
}
