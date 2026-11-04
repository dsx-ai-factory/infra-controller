// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"net/http"
	"slices"

	"github.com/labstack/echo/v4"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// SiteExplorerEndpointActionHandler triggers clear-error or re-explore actions for explored endpoints.
type SiteExplorerEndpointActionHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewSiteExplorerEndpointActionHandler returns a handler for site-explorer endpoint actions.
func NewSiteExplorerEndpointActionHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) SiteExplorerEndpointActionHandler {
	return SiteExplorerEndpointActionHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Trigger Site Explorer Endpoint Action
// @Description Trigger clear-error or re-explore for all or selected explored endpoints.
// @Tags site-explorer
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param request body model.APISiteExplorerEndpointActionRequest true "Site explorer endpoint action"
// @Success 200 {object} model.APISiteExplorerEndpointAction
// @Router /v2/org/{org}/nico/site-explorer/endpoint/action [post]
func (h SiteExplorerEndpointActionHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("SiteExplorerEndpointAction", "Create", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiReq model.APISiteExplorerEndpointActionRequest
	if err := c.Bind(&apiReq); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid request body", nil)
	}
	if err := apiReq.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	stc, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx:       ctx,
		Logger:    logger,
		DBSession: h.dbSession,
		SCP:       h.scp,
		Org:       org,
		User:      dbUser,
		SiteID:    apiReq.SiteID,
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	endpointIDs := slices.Clone(apiReq.EndpointIDs)
	if apiReq.Target == model.SiteExplorerEndpointTargetAll {
		var ids corev1.ExploredEndpointIdList
		apiErr = common.ExecuteCoreGRPC(
			ctx,
			stc,
			corev1.Forge_FindExploredEndpointIds_FullMethodName,
			&corev1.ExploredEndpointSearchFilter{},
			&ids,
			siteID,
		)
		if apiErr != nil {
			logAPIError(logger, apiErr, "failed to find explored endpoint IDs")
			return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
		}
		endpointIDs = ids.GetEndpointIds()
	}

	logger.Info().
		Str("action", apiReq.Action).
		Str("target", apiReq.Target).
		Str("siteID", apiReq.SiteID).
		Int("endpointCount", len(endpointIDs)).
		Msg("triggering site-explorer endpoint action via Core proxy")

	for _, endpointID := range endpointIDs {
		var fullMethod string
		var coreReq proto.Message
		switch apiReq.Action {
		case model.SiteExplorerEndpointActionClearError:
			fullMethod = corev1.Forge_ClearSiteExplorationError_FullMethodName
			coreReq = &corev1.ClearSiteExplorationErrorRequest{IpAddress: endpointID}
		case model.SiteExplorerEndpointActionReExplore:
			fullMethod = corev1.Forge_ReExploreEndpoint_FullMethodName
			coreReq = &corev1.ReExploreEndpointRequest{IpAddress: endpointID}
		}

		apiErr = common.ExecuteCoreGRPC(ctx, stc, fullMethod, coreReq, nil, siteID)
		if apiErr != nil {
			actionLogger := logger.With().Str("action", apiReq.Action).Str("endpointID", endpointID).Logger()
			logAPIError(actionLogger, apiErr, "failed to trigger site-explorer endpoint action")
			return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
		}
	}

	return c.JSON(http.StatusOK, apiReq.ToResponse(endpointIDs))
}
