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

// DecommissionPowerShelfHandler starts decommissioning a managed power shelf.
type DecommissionPowerShelfHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewDecommissionPowerShelfHandler returns a power shelf decommission handler.
func NewDecommissionPowerShelfHandler(dbSession *cdb.Session, scp *sc.ClientPool) DecommissionPowerShelfHandler {
	return DecommissionPowerShelfHandler{dbSession: dbSession, scp: scp}
}

// Handle godoc
// @Summary Decommission a Power Shelf
// @Description Start decommissioning a Ready managed power shelf through Core.
// @Tags PowerShelf
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of the Org"
// @Param id path string true "Core inventory ID of Power Shelf"
// @Param request body model.APIPowerShelfDecommissionRequest true "Site containing the power shelf"
// @Success 202 {object} model.APIMessageResponse
// @Router /v2/org/{org}/nico/power-shelf/{id}/decommission [post]
func (h DecommissionPowerShelfHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("PowerShelf", "Decommission", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	var request model.APIPowerShelfDecommissionRequest
	err := c.Bind(&request)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	request.PowerShelfID = c.Param("id")
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
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_DecommissionPowerShelf_FullMethodName, request.ToProto(), nil, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "Failed to start Power Shelf decommissioning via Core gRPC proxy")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	return c.JSON(http.StatusAccepted, model.APIMessageResponse{Message: "Power Shelf decommissioning request was accepted"})
}
