// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"net/http"

	"go.opentelemetry.io/otel/attribute"
	temporalClient "go.temporal.io/sdk/client"

	"github.com/google/uuid"

	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
)

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllFabricHandler is the API Handler for getting all Fabrics
type GetAllFabricHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetAllFabricHandler initializes and returns a new handler for getting all Fabrics
func NewGetAllFabricHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllFabricHandler {
	return GetAllFabricHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Retrieve all Fabrics
// @Description Retrieve all Fabrics for Sites owned by the org. Endpoint is currently not exposed to users
// @Tags Fabric
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param includeRelation query string false "Related entities to include in response e.g. 'InfrastructureProvider', 'Site'"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APIFabric
// @Router /v2/org/{org}/nico/site/{siteId}/fabric [get]
func (gafh GetAllFabricHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Fabric", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	provider, tenant, apiErr := common.IsProviderOrTenant(ctx, logger, gafh.dbSession, org, dbUser, true, nil)
	if apiErr != nil {
		return c.JSON(apiErr.Code, apiErr)
	}

	// Validate pagination request
	pageRequest := pagination.PageRequest{}
	err := c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate request attributes
	err = pageRequest.Validate(cdbm.FabricOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Get Site ID from url
	stStrID := c.Param("siteId")

	cotel.SetAttribute(handlerSpan, attribute.String("site_id", stStrID))

	stID, err := uuid.Parse(stStrID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Site ID in URL", nil)
	}

	// Get Site
	stDAO := cdbm.NewSiteDAO(gafh.dbSession)
	st, err := stDAO.GetByID(ctx, nil, stID, nil, false)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in query", nil)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.FabricRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	isAssociated := false
	if provider != nil {
		isAssociated = provider.ID == st.InfrastructureProviderID
	}
	tsDAO := cdbm.NewTenantSiteDAO(gafh.dbSession)
	if !isAssociated && tenant != nil {
		_, serr := tsDAO.GetByTenantIDAndSiteID(ctx, nil, tenant.ID, stID, nil)
		if serr != nil {
			if serr != cdb.ErrDoesNotExist {
				logger.Error().Err(serr).Msg("error retrieving TenantSite from DB")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to determine org's association with Site, DB error", nil)
			}
		} else {
			isAssociated = true
		}
	}
	if !isAssociated {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site is not associated with org", nil)
	}

	// Get query text for full text search from query param
	searchQuery := common.GetSearchQuery(c)
	if searchQuery != nil {
		cotel.SetAttribute(handlerSpan, attribute.String("query", *searchQuery))
	}

	fbDAO := cdbm.NewFabricDAO(gafh.dbSession)
	dbfbs, total, err := fbDAO.GetAll(ctx, nil, cdbm.FabricFilterInput{
		Org:         &org,
		SiteIDs:     []uuid.UUID{st.ID},
		SearchQuery: searchQuery,
	}, cdbp.PageInput{
		Offset:  pageRequest.Offset,
		Limit:   pageRequest.Limit,
		OrderBy: pageRequest.OrderBy,
	}, qIncludeRelations)
	if err != nil {
		logger.Error().Err(err).Msg("error getting Fabrics from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Fabrics", nil)
	}

	// Get status details
	sdDAO := cdbm.NewStatusDetailDAO(gafh.dbSession)

	sdEntityIDs := []string{}
	for _, fb := range dbfbs {
		sdEntityIDs = append(sdEntityIDs, fb.ID)
	}
	ssds, serr := sdDAO.GetRecentByEntityIDs(ctx, nil, sdEntityIDs, common.RECENT_STATUS_DETAIL_COUNT)
	if serr != nil {
		logger.Warn().Err(serr).Msg("error retrieving Status Details for Fabrics from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate status history for Fabrics", nil)
	}
	ssdMap := map[string][]cdbm.StatusDetail{}
	for _, ssd := range ssds {
		cssd := ssd
		ssdMap[ssd.EntityID] = append(ssdMap[ssd.EntityID], cssd)
	}

	// Create response
	apiFabrics := []model.APIFabric{}
	for _, fb := range dbfbs {
		dbfb := fb
		apiFabric := model.NewAPIFabric(&dbfb, ssdMap[fb.ID])
		apiFabrics = append(apiFabrics, *apiFabric)
	}

	// Create pagination response header
	pageReponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageReponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}

	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiFabrics)
}

// ~~~~~ Get Handler ~~~~~ //

// GetFabricHandler is the API Handler for retrieving Fabric
type GetFabricHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetFabricHandler initializes and returns a new handler to retrieve Fabric
func NewGetFabricHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetFabricHandler {
	return GetFabricHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Retrieve Fabric
// @Description Retrieve Fabric by ID. Endpoint is currently not exposed to users
// @Tags Fabric
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Fabric"
// @Param siteId query string true "ID of Site"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Site'"
// @Success 200 {object} model.APIFabric
// @Router /v2/org/{org}/nico/site/{siteId}/fabric/{id} [get]
func (gfh GetFabricHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Fabric", "Get", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	provider, tenant, apiErr := common.IsProviderOrTenant(ctx, logger, gfh.dbSession, org, dbUser, true, nil)
	if apiErr != nil {
		return c.JSON(apiErr.Code, apiErr)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.FabricRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Get Site ID URL param
	stStrID := c.Param("siteId")

	cotel.SetAttribute(handlerSpan, attribute.String("site_id", stStrID))

	stID, err := uuid.Parse(stStrID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Site ID in URL", nil)
	}

	// Get Site
	stDAO := cdbm.NewSiteDAO(gfh.dbSession)
	st, err := stDAO.GetByID(ctx, nil, stID, nil, false)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in query", nil)
	}

	// Get fabric ID from URL param
	fID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("fabric_id", fID))

	fbDAO := cdbm.NewFabricDAO(gfh.dbSession)
	// Check that Fabric exists
	fb, err := fbDAO.GetByID(ctx, nil, fID, stID, qIncludeRelations)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Fabric with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Fabric DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Could not retrieve Fabric", nil)
	}

	isAssociated := false
	if provider != nil {
		isAssociated = provider.ID == st.InfrastructureProviderID
	}
	tsDAO := cdbm.NewTenantSiteDAO(gfh.dbSession)
	if !isAssociated && tenant != nil {
		_, serr := tsDAO.GetByTenantIDAndSiteID(ctx, nil, tenant.ID, stID, nil)
		if serr != nil {
			if serr != cdb.ErrDoesNotExist {
				logger.Error().Err(serr).Msg("error retrieving TenantSite from DB")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to determine org's association with Site, DB error", nil)
			}
		} else {
			isAssociated = true
		}
	}
	if !isAssociated {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site is not associated with org", nil)
	}

	// Create response
	// Get status details
	sdDAO := cdbm.NewStatusDetailDAO(gfh.dbSession)
	ssds, err := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{fb.ID}, common.RECENT_STATUS_DETAIL_COUNT)
	if err != nil {
		logger.Warn().Err(err).Msg("error retrieving Status Details for Fabrics from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate status history for Fabrics", nil)
	}
	ssdMap := map[string][]cdbm.StatusDetail{}
	for _, ssd := range ssds {
		cssd := ssd
		ssdMap[ssd.EntityID] = append(ssdMap[ssd.EntityID], cssd)
	}
	apiFabric := model.NewAPIFabric(fb, ssds)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiFabric)
}
