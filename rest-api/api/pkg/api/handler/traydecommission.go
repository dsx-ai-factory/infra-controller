// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"net/http"

	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// DecommissionTrayHandler starts decommissioning a managed tray.
type DecommissionTrayHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewDecommissionTrayHandler returns a tray decommission handler.
func NewDecommissionTrayHandler(dbSession *cdb.Session, scp *sc.ClientPool) DecommissionTrayHandler {
	return DecommissionTrayHandler{dbSession: dbSession, scp: scp}
}

// Handle godoc
// @Summary Decommission a Tray
// @Description Start decommissioning a Ready managed tray through Core.
// @Tags Tray
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of the Org"
// @Param id path string true "Core inventory ID of Tray"
// @Param request body model.APITrayDecommissionRequest true "Site containing the tray"
// @Success 202 {object} model.APIMessageResponse
// @Router /v2/org/{org}/nico/tray/{id}/decommission [post]
func (h DecommissionTrayHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Tray", "Decommission", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	var request model.APITrayDecommissionRequest
	err := c.Bind(&request)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	request.TrayID = c.Param("id")
	err = request.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	stc, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx: ctx, Logger: logger, DBSession: h.dbSession, SCP: h.scp,
		Org: org, User: dbUser, SiteID: request.SiteID,
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	// Core owns readiness and rack-assignment checks. Do not retry automatically:
	// a timeout does not prove that Core rejected the request.
	coreRequest := request.ToProto()
	var method string
	switch coreRequest.(type) {
	case *corev1.DecommissionManagedHostRequest:
		method = corev1.Forge_DecommissionManagedHost_FullMethodName
	case *corev1.DecommissionSwitchRequest:
		method = corev1.Forge_DecommissionSwitch_FullMethodName
	case *corev1.DecommissionPowerShelfRequest:
		method = corev1.Forge_DecommissionPowerShelf_FullMethodName
	}
	apiErr = common.ExecuteCoreGRPC(ctx, stc, method, coreRequest, nil, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "Failed to start Tray decommissioning via Core gRPC proxy")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	return c.JSON(http.StatusAccepted, model.APIMessageResponse{Message: "Tray decommissioning request was accepted"})
}
