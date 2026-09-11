// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"net/http"
	"sort"

	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	exploredEndpointOrderByFieldID = "id"
	exploredEndpointOrderByIDAsc   = "ID_ASC"
	exploredEndpointOrderByIDDesc  = "ID_DESC"
)

// GetAllExploredEndpointHandler lists explored endpoints for a Site via Core.
type GetAllExploredEndpointHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewGetAllExploredEndpointHandler returns a handler for listing explored endpoints.
func NewGetAllExploredEndpointHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) GetAllExploredEndpointHandler {
	return GetAllExploredEndpointHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Retrieve all Explored Endpoints
// @Description Retrieve explored endpoints discovered by Site Explorer for a Site, ordered by endpoint ID.
// @Tags site-explorer
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of Site"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Endpoint ID ordering" Enums(ID_ASC, ID_DESC) default(ID_ASC)
// @Success 200 {array} model.APIExploredEndpoint
// @Router /v2/org/{org}/nico/site-explorer/endpoint [get]
func (h GetAllExploredEndpointHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExploredEndpoint", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiRequest := model.APIExploredEndpointGetAllRequest{}
	if err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest, pagination.PageRequest{}); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := apiRequest.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	pageRequest := pagination.PageRequest{}
	if err := c.Bind(&pageRequest); err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}
	if pageRequest.OrderByStr == nil {
		pageRequest.OrderByStr = cutil.GetPtr(exploredEndpointOrderByIDAsc)
	}
	if err := pageRequest.Validate([]string{exploredEndpointOrderByFieldID}); err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	stc, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx:       ctx,
		Logger:    logger,
		DBSession: h.dbSession,
		SCP:       h.scp,
		Org:       org,
		User:      dbUser,
		SiteID:    apiRequest.SiteID,
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

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
		logAPIError(logger, apiErr, "failed to retrieve explored endpoint IDs from Site")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	allIDs := append([]string(nil), ids.GetEndpointIds()...)
	if *pageRequest.OrderByStr == exploredEndpointOrderByIDDesc {
		sort.Sort(sort.Reverse(sort.StringSlice(allIDs)))
	} else {
		sort.Strings(allIDs)
	}
	total := len(allIDs)

	start := *pageRequest.Offset
	if start > total {
		start = total
	}
	end := start + *pageRequest.Limit
	if end > total {
		end = total
	}
	pageIDs := allIDs[start:end]

	apiEndpoints := make([]*model.APIExploredEndpoint, 0, len(pageIDs))
	if len(pageIDs) > 0 {
		var endpointList corev1.ExploredEndpointList
		apiErr = common.ExecuteCoreGRPC(
			ctx,
			stc,
			corev1.Forge_FindExploredEndpointsByIds_FullMethodName,
			&corev1.ExploredEndpointsByIdsRequest{EndpointIds: pageIDs},
			&endpointList,
			siteID,
		)
		if apiErr != nil {
			logAPIError(logger, apiErr, "failed to find explored endpoints by IDs")
			return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
		}
		endpointsByID := make(map[string]*corev1.ExploredEndpoint, len(endpointList.GetEndpoints()))
		for _, endpoint := range endpointList.GetEndpoints() {
			endpointsByID[endpoint.GetAddress()] = endpoint
		}
		for _, endpointID := range pageIDs {
			if endpoint, ok := endpointsByID[endpointID]; ok {
				apiEndpoints = append(apiEndpoints, model.NewAPIExploredEndpoint(endpoint))
			}
		}
	}

	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().
		Str("siteID", siteID).
		Int("total", total).
		Int("returned", len(apiEndpoints)).
		Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiEndpoints)
}
