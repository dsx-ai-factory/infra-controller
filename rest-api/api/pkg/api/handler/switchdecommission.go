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

// DecommissionSwitchHandler starts decommissioning a managed switch.
type DecommissionSwitchHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewDecommissionSwitchHandler returns a switch decommission handler.
func NewDecommissionSwitchHandler(dbSession *cdb.Session, scp *sc.ClientPool) DecommissionSwitchHandler {
	return DecommissionSwitchHandler{dbSession: dbSession, scp: scp}
}

// Handle godoc
// @Summary Decommission a Switch
// @Description Start decommissioning a Ready managed switch through Core.
// @Tags Switch
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of the Org"
// @Param id path string true "Core inventory ID of Switch"
// @Param request body model.APISwitchDecommissionRequest true "Site containing the switch"
// @Success 202 {object} model.APIMessageResponse
// @Router /v2/org/{org}/nico/switch/{id}/decommission [post]
func (h DecommissionSwitchHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Switch", "Decommission", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	var request model.APISwitchDecommissionRequest
	err := c.Bind(&request)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	request.SwitchID = c.Param("id")
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
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_DecommissionSwitch_FullMethodName, request.ToProto(), nil, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "Failed to start Switch decommissioning via Core gRPC proxy")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	return c.JSON(http.StatusAccepted, model.APIMessageResponse{Message: "Switch decommissioning request was accepted"})
}
