// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"

	"go.opentelemetry.io/otel/attribute"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	temporalEnums "go.temporal.io/api/enums/v1"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// ~~~~~ Create Handler ~~~~~ //

// CreateDhcpOptionSetHandler is the API Handler for creating a new DhcpOptionSet
type CreateDhcpOptionSetHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewCreateDhcpOptionSetHandler returns a handler for creating a DhcpOptionSet
func NewCreateDhcpOptionSetHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) CreateDhcpOptionSetHandler {
	return CreateDhcpOptionSetHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Create a DhcpOptionSet
// @Description Create a DhcpOptionSet
// @Tags DhcpOptionSet
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIDhcpOptionSetCreateRequest true "DhcpOptionSet creation request"
// @Success 201 {object} model.APIDhcpOptionSet
// @Router /v2/org/{org}/nico/dhcp-option-set [post]
func (cdosh CreateDhcpOptionSetHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("DhcpOptionSet", "Create", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	orgTenant, apiErr := common.IsTenant(ctx, logger, cdosh.dbSession, org, dbUser, nil)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	// Bind request data to API model
	apiRequest := model.APIDhcpOptionSetCreateRequest{}
	err := c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating DHCP Option Set creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating DHCP Option Set creation request data", verr)
	}

	// Validate and verify the Site is ready
	site, serr := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, cdosh.dbSession)
	if serr != nil {
		if serr == common.ErrInvalidID {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create DHCP Option Set, Invalid Site ID: %s", apiRequest.SiteID), nil)
		}
		if errors.Is(serr, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request data does not exist", nil)
		}
		logger.Error().Err(serr).Str("Site ID", apiRequest.SiteID).Msg("error retrieving Site from DB by ID")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request data due to DB error", nil)
	}

	if site.Status != cdbm.SiteStatusRegistered {
		logger.Warn().Str("Site ID", site.ID.String()).Msg("Site is not in Registered state")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create DHCP Option Set, Site: %s specified in request is not in Registered state", site.ID.String()), nil)
	}

	// Ensure that the Tenant has access to the specified Site
	hasSiteAccess, serr := common.TenantHasSiteAccess(ctx, nil, cdosh.dbSession, orgTenant, site.ID)
	if serr != nil {
		logger.Error().Err(serr).Msg("error checking Tenant access to Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to determine Tenant access to Site, DB error", nil)
	}
	if !hasSiteAccess {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have access to Site specified in request data", nil)
	}

	// Core rejects a DHCP Option Set until the Tenant exists on the Site.
	err = ensurePrivilegedTenantSites(ctx, cdosh.dbSession, cdosh.scp, logger, orgTenant, dbUser.ID, site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("error provisioning Tenant on Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to provision Tenant on Site", nil)
	}

	stc, derr := cdosh.scp.GetClientByID(site.ID)
	if derr != nil {
		logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	dosDAO := cdbm.NewDhcpOptionSetDAO(cdosh.dbSession)
	sdDAO := cdbm.NewStatusDetailDAO(cdosh.dbSession)

	var createdDOS *cdbm.DhcpOptionSet
	var ssd *cdbm.StatusDetail

	err = cdb.WithTx(ctx, cdosh.dbSession, func(tx *cdb.Tx) error {
		// Acquire a lock so that another concurrent request with the same name cannot succeed.
		lockKey := fmt.Sprintf("dhcp-option-set-name:%s:%s:%s", orgTenant.ID, site.ID, apiRequest.Name)
		derr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(lockKey), nil)
		if derr != nil {
			if errors.Is(derr, cdb.ErrXactAdvisoryLockFailed) {
				logger.Warn().Err(derr).Str("name", apiRequest.Name).Msg("concurrent DHCP Option Set create detected")
				return cutil.NewAPIError(http.StatusConflict, "Creation of a DHCP Option Set with specified name is already in progress in another request", nil)
			}
			logger.Error().Err(derr).Msg("unable to acquire advisory lock for DHCP Option Set creation")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create DHCP Option Set due to DB error", nil)
		}

		existing, tot, derr := dosDAO.GetAll(ctx, tx, cdbm.DhcpOptionSetFilterInput{
			Names:     []string{apiRequest.Name},
			SiteIDs:   []uuid.UUID{site.ID},
			TenantIDs: []uuid.UUID{orgTenant.ID},
		}, paginator.PageInput{}, nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("db error checking for name uniqueness of tenant DHCP Option Set")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create DHCP Option Set due to DB error", nil)
		}
		if tot > 0 {
			// The count and the row scan are separate queries, so a concurrent
			// delete between them can report a match with no row to name.
			var conflictData validation.Errors
			if len(existing) > 0 {
				conflictData = validation.Errors{"id": errors.New(existing[0].ID.String())}
			}
			return cutil.NewAPIError(http.StatusConflict, "Another DHCP Option Set with specified name already exists for Tenant on Site", conflictData)
		}

		dosID := uuid.New()
		logger.Info().Str("DHCP Option Set ID", dosID.String()).Msg("creating DHCP Option Set via Core proxy")

		coreReq := apiRequest.ToProto(dosID, org)
		coreResp := &corev1.CreateDhcpOptionSetResponse{}
		proxyErr := common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_CreateDhcpOptionSet_FullMethodName, coreReq, coreResp, "")
		if proxyErr != nil {
			logAPIError(logger, proxyErr, "failed to create DHCP Option Set on Site")
			return cutil.NewAPIError(proxyErr.Code, proxyErr.Message, nil)
		}

		// Core's create is complete when it returns, so record the set as Ready with its version in a single write.
		dos, derr := dosDAO.Create(ctx, tx, cdbm.DhcpOptionSetCreateInput{
			DhcpOptionSetID: &dosID,
			Name:            apiRequest.Name,
			Description:     apiRequest.Description,
			TenantOrg:       org,
			SiteID:          site.ID,
			TenantID:        orgTenant.ID,
			Options:         &cdbm.DhcpOptions{DhcpOptions: coreReq.Options},
			Labels:          apiRequest.Labels,
			Status:          cdbm.DhcpOptionSetStatusReady,
			Version:         cutil.GetPtr(coreResp.GetDhcpOptionSet().GetVersion()),
			CreatedBy:       dbUser.ID,
		})
		if derr != nil {
			logger.Error().Err(derr).Msg("unable to create DHCP Option Set record in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed creating DHCP Option Set record", nil)
		}

		ssd, derr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{
			EntityID: dos.ID.String(),
			Status:   string(cdbm.DhcpOptionSetStatusReady),
			Message:  cutil.GetPtr("DHCP Option Set was created on Site"),
		})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for DHCP Option Set", nil)
		}

		createdDOS = dos
		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to create DHCP Option Set, DB transaction error")
	}

	apiDOS := &model.APIDhcpOptionSet{}
	apiDOS.FromDBModel(createdDOS, []cdbm.StatusDetail{*ssd})

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusCreated, apiDOS)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllDhcpOptionSetHandler is the API Handler for retrieving all DhcpOptionSets
type GetAllDhcpOptionSetHandler struct {
	dbSession *cdb.Session
	cfg       *config.Config
}

// NewGetAllDhcpOptionSetHandler returns a handler for retrieving all DhcpOptionSets
func NewGetAllDhcpOptionSetHandler(dbSession *cdb.Session, cfg *config.Config) GetAllDhcpOptionSetHandler {
	return GetAllDhcpOptionSetHandler{
		dbSession: dbSession,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get all DhcpOptionSets
// @Description Get all DhcpOptionSets
// @Tags DhcpOptionSet
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string false "ID of Site"
// @Param status query string false "Filter by status e.g. 'Ready'"
// @Param query query string false "Query input for full text search"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Site', 'Tenant'"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APIDhcpOptionSet
// @Router /v2/org/{org}/nico/dhcp-option-set [get]
func (gadosh GetAllDhcpOptionSetHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("DhcpOptionSet", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	tenant, apiErr := common.IsTenant(ctx, logger, gadosh.dbSession, org, dbUser, nil)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	apiRequest := model.APIDhcpOptionSetGetAllRequest{}
	pageRequest := pagination.PageRequest{}
	err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest, pageRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding query parameters into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request query parameters", nil)
	}

	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	err = pageRequest.Validate(cdbm.DhcpOptionSetOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	qParams := c.QueryParams()

	// The siteId parameter repeats to filter on more than one Site, and the Tenant has to have access to every one of them.
	var siteIDs []uuid.UUID
	if len(apiRequest.SiteID) > 0 {
		cotel.SetAttribute(handlerSpan, attribute.StringSlice("siteId", apiRequest.SiteID))
		for _, siteIDStr := range apiRequest.SiteID {
			site, err := common.GetSiteFromIDString(ctx, nil, siteIDStr, gadosh.dbSession)
			if err != nil {
				if errors.Is(err, common.ErrInvalidID) || errors.Is(err, cdb.ErrDoesNotExist) {
					return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Site %v specified in query param does not exist", siteIDStr), nil)
				}
				logger.Error().Err(err).Msg("error retrieving Site from DB")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in query param due to DB error", nil)
			}

			err = common.ValidateTenantSiteAccess(ctx, nil, gadosh.dbSession, tenant, site.ID)
			if err != nil {
				if errors.Is(err, cdb.ErrDoesNotExist) {
					return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Tenant does not have access to Site %v", siteIDStr), nil)
				}
				logger.Error().Err(err).Msg("error checking Tenant access to Site")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to determine Tenant access to Site, DB error", nil)
			}

			siteIDs = append(siteIDs, site.ID)
		}
	}

	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.DhcpOptionSetRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	searchQuery := common.GetSearchQuery(c)
	if searchQuery != nil {
		cotel.SetAttribute(handlerSpan, attribute.String("query", *searchQuery))
	}

	// The status parameter repeats to match more than one Status.
	var statuses []string
	if len(apiRequest.Status) > 0 {
		cotel.SetAttribute(handlerSpan, attribute.StringSlice("status", apiRequest.Status))
		for _, status := range apiRequest.Status {
			if !cdbm.DhcpOptionSetStatusMap[cdbm.DhcpOptionSetStatus(status)] {
				logger.Warn().Str("status", status).Msg("invalid value in status query")
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid Status value %v in query", status), nil)
			}
			statuses = append(statuses, status)
		}
	}

	dosDAO := cdbm.NewDhcpOptionSetDAO(gadosh.dbSession)
	doss, total, err := dosDAO.GetAll(
		ctx,
		nil,
		cdbm.DhcpOptionSetFilterInput{
			SiteIDs:     siteIDs,
			TenantIDs:   []uuid.UUID{tenant.ID},
			Statuses:    statuses,
			SearchQuery: searchQuery,
		},
		paginator.PageInput{
			Offset:  pageRequest.Offset,
			Limit:   pageRequest.Limit,
			OrderBy: pageRequest.OrderBy,
		},
		qIncludeRelations,
	)
	if err != nil {
		logger.Error().Err(err).Msg("error getting DHCP Option Sets from db")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve DHCP Option Sets, DB error", nil)
	}

	sdDAO := cdbm.NewStatusDetailDAO(gadosh.dbSession)
	sdEntityIDs := []string{}
	for _, dos := range doss {
		sdEntityIDs = append(sdEntityIDs, dos.ID.String())
	}

	ssds, serr := sdDAO.GetRecentByEntityIDs(ctx, nil, sdEntityIDs, common.RECENT_STATUS_DETAIL_COUNT)
	if serr != nil {
		logger.Warn().Err(serr).Msg("error retrieving Status Details for DHCP Option Sets from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate status history for DHCP Option Sets", nil)
	}

	ssdMap := map[string][]cdbm.StatusDetail{}
	for _, ssd := range ssds {
		cssd := ssd
		ssdMap[ssd.EntityID] = append(ssdMap[ssd.EntityID], cssd)
	}

	apiDOSs := []*model.APIDhcpOptionSet{}
	for _, dos := range doss {
		curDOS := dos
		apiDOS := &model.APIDhcpOptionSet{}
		apiDOS.FromDBModel(&curDOS, ssdMap[dos.ID.String()])
		apiDOSs = append(apiDOSs, apiDOS)
	}

	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}

	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiDOSs)
}

// ~~~~~ Get Handler ~~~~~ //

// GetDhcpOptionSetHandler is the API Handler for retrieving a DhcpOptionSet
type GetDhcpOptionSetHandler struct {
	dbSession *cdb.Session
	cfg       *config.Config
}

// NewGetDhcpOptionSetHandler returns a handler for retrieving a DhcpOptionSet
func NewGetDhcpOptionSetHandler(dbSession *cdb.Session, cfg *config.Config) GetDhcpOptionSetHandler {
	return GetDhcpOptionSetHandler{
		dbSession: dbSession,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get a DhcpOptionSet
// @Description Get a DhcpOptionSet
// @Tags DhcpOptionSet
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param dhcpOptionSetId path string true "ID of DhcpOptionSet"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Site', 'Tenant'"
// @Success 200 {object} model.APIDhcpOptionSet
// @Router /v2/org/{org}/nico/dhcp-option-set/{dhcpOptionSetId} [get]
func (gdosh GetDhcpOptionSetHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("DhcpOptionSet", "Get", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	orgTenant, apiErr := common.IsTenant(ctx, logger, gdosh.dbSession, org, dbUser, nil)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	dosID, err := uuid.Parse(c.Param("id"))
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid DHCP Option Set ID in URL", nil)
	}

	qParams := c.QueryParams()
	apiRequest := model.APIDhcpOptionSetGetRequest{}
	err = common.ValidateKnownQueryParams(qParams, apiRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding query parameters into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request query parameters", nil)
	}

	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.DhcpOptionSetRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	dosDAO := cdbm.NewDhcpOptionSetDAO(gdosh.dbSession)
	dos, err := dosDAO.GetByID(ctx, nil, dosID, qIncludeRelations)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find DHCP Option Set with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving DHCP Option Set DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve DHCP Option Set, DB error", nil)
	}

	if dos.TenantID != orgTenant.ID {
		logger.Warn().Msg("Tenant in DHCP Option Set does not belong to Tenant in org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant for DHCP Option Set in request does not match Tenant in org", nil)
	}

	sdDAO := cdbm.NewStatusDetailDAO(gdosh.dbSession)
	ssds, serr := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{dos.ID.String()}, common.RECENT_STATUS_DETAIL_COUNT)
	if serr != nil {
		logger.Warn().Err(serr).Msg("error retrieving Status Details for DHCP Option Set from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate status history for DHCP Option Set", nil)
	}

	apiDOS := &model.APIDhcpOptionSet{}
	apiDOS.FromDBModel(dos, ssds)

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiDOS)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateDhcpOptionSetHandler is the API Handler for updating a DhcpOptionSet
type UpdateDhcpOptionSetHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewUpdateDhcpOptionSetHandler returns a handler for updating a DhcpOptionSet
func NewUpdateDhcpOptionSetHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) UpdateDhcpOptionSetHandler {
	return UpdateDhcpOptionSetHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Update a DhcpOptionSet
// @Description Update a DhcpOptionSet
// @Tags DhcpOptionSet
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param dhcpOptionSetId path string true "ID of DhcpOptionSet"
// @Param message body model.APIDhcpOptionSetUpdateRequest true "DhcpOptionSet update request"
// @Success 200 {object} model.APIDhcpOptionSet
// @Router /v2/org/{org}/nico/dhcp-option-set/{dhcpOptionSetId} [patch]
func (udosh UpdateDhcpOptionSetHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("DhcpOptionSet", "Update", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	orgTenant, apiErr := common.IsTenant(ctx, logger, udosh.dbSession, org, dbUser, nil)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	dosID, err := uuid.Parse(c.Param("id"))
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid DHCP Option Set ID in URL", nil)
	}

	// Bind request data to API model
	apiRequest := model.APIDhcpOptionSetUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating DHCP Option Set update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating DHCP Option Set update request data", verr)
	}

	dosDAO := cdbm.NewDhcpOptionSetDAO(udosh.dbSession)
	dos, err := dosDAO.GetByID(ctx, nil, dosID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find DHCP Option Set with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving DHCP Option Set DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve DHCP Option Set, DB error", nil)
	}

	if dos.TenantID != orgTenant.ID {
		logger.Warn().Msg("Tenant in DHCP Option Set does not belong to Tenant in org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant for DHCP Option Set in request does not match Tenant in org", nil)
	}

	// Ensure that Tenant has access to the DHCP Option Set's Site
	hasSiteAccess, err := common.TenantHasSiteAccess(ctx, nil, udosh.dbSession, orgTenant, dos.SiteID)
	if err != nil {
		logger.Error().Err(err).Msg("error checking Tenant access to Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to determine Tenant access to Site, DB error", nil)
	}
	if !hasSiteAccess {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have access to Site for DHCP Option Set", nil)
	}

	stc, derr := udosh.scp.GetClientByID(dos.SiteID)
	if derr != nil {
		logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	var updatedDOS *cdbm.DhcpOptionSet

	err = cdb.WithTx(ctx, udosh.dbSession, func(tx *cdb.Tx) error {
		// Serialize updates and deletes of this set so each one acts on the latest stored values and version.
		derr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString("dhcp-option-set:"+dos.ID.String()), nil)
		if derr != nil {
			if errors.Is(derr, cdb.ErrXactAdvisoryLockFailed) {
				logger.Warn().Err(derr).Msg("concurrent DHCP Option Set change detected")
				return cutil.NewAPIError(http.StatusConflict, "Another request is changing this DHCP Option Set, retry the request", nil)
			}
			logger.Error().Err(derr).Msg("unable to acquire advisory lock for DHCP Option Set update")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update DHCP Option Set due to DB error", nil)
		}

		current, derr := dosDAO.GetByID(ctx, tx, dos.ID, nil)
		if derr != nil {
			if errors.Is(derr, cdb.ErrDoesNotExist) {
				return cutil.NewAPIError(http.StatusNotFound, "Could not find DHCP Option Set with specified ID", nil)
			}
			logger.Error().Err(derr).Msg("error retrieving DHCP Option Set DB entity")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve DHCP Option Set, DB error", nil)
		}

		// Hold the same name lock as create so a rename can't race another create or rename to that name.
		if apiRequest.Name != nil && *apiRequest.Name != current.Name {
			nameLockKey := fmt.Sprintf("dhcp-option-set-name:%s:%s:%s", current.TenantID, current.SiteID, *apiRequest.Name)
			derr = tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(nameLockKey), nil)
			if derr != nil {
				if errors.Is(derr, cdb.ErrXactAdvisoryLockFailed) {
					logger.Warn().Err(derr).Str("name", *apiRequest.Name).Msg("concurrent DHCP Option Set rename detected")
					return cutil.NewAPIError(http.StatusConflict, "Another request is using the specified name, retry the request", nil)
				}
				logger.Error().Err(derr).Msg("unable to acquire advisory lock for DHCP Option Set rename")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update DHCP Option Set due to DB error", nil)
			}

			existing, _, derr := dosDAO.GetAll(ctx, tx, cdbm.DhcpOptionSetFilterInput{
				Names:     []string{*apiRequest.Name},
				SiteIDs:   []uuid.UUID{current.SiteID},
				TenantIDs: []uuid.UUID{current.TenantID},
			}, paginator.PageInput{}, nil)
			if derr != nil {
				logger.Error().Err(derr).Msg("db error checking for name uniqueness of tenant DHCP Option Set")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to check for existing DHCP Option Set, DB error", nil)
			}
			if len(existing) > 0 {
				return cutil.NewAPIError(http.StatusConflict, "Another DHCP Option Set with specified name already exists for Tenant on Site", validation.Errors{
					"id": errors.New(existing[0].ID.String()),
				})
			}
		}

		logger.Info().Str("DHCP Option Set ID", dos.ID.String()).Msg("updating DHCP Option Set via Core proxy")

		coreReq := apiRequest.ToProto(current)
		coreResp := &corev1.UpdateDhcpOptionSetResponse{}
		proxyErr := common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_UpdateDhcpOptionSet_FullMethodName, coreReq, coreResp, "")
		if proxyErr != nil {
			logAPIError(logger, proxyErr, "failed to update DHCP Option Set on Site")
			return cutil.NewAPIError(proxyErr.Code, proxyErr.Message, nil)
		}

		// Core has committed, so record its values and new version in a single write.
		updatedDOS, derr = dosDAO.Update(ctx, tx, cdbm.DhcpOptionSetUpdateInput{
			DhcpOptionSetID: dos.ID,
			Name:            apiRequest.Name,
			Description:     apiRequest.Description,
			Options:         &cdbm.DhcpOptions{DhcpOptions: coreReq.Options},
			Labels:          apiRequest.Labels,
			Version:         cutil.GetPtr(coreResp.GetDhcpOptionSet().GetVersion()),
		})
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating DHCP Option Set in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update DHCP Option Set, DB error", nil)
		}
		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to update DHCP Option Set, DB transaction error")
	}

	sdDAO := cdbm.NewStatusDetailDAO(udosh.dbSession)
	ssds, serr := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{updatedDOS.ID.String()}, common.RECENT_STATUS_DETAIL_COUNT)
	if serr != nil {
		logger.Warn().Err(serr).Msg("error retrieving Status Details for DHCP Option Set from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate status history for DHCP Option Set", nil)
	}

	apiDOS := &model.APIDhcpOptionSet{}
	apiDOS.FromDBModel(updatedDOS, ssds)

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiDOS)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteDhcpOptionSetHandler is the API Handler for deleting a DhcpOptionSet
type DeleteDhcpOptionSetHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteDhcpOptionSetHandler returns a handler for deleting a DhcpOptionSet
func NewDeleteDhcpOptionSetHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) DeleteDhcpOptionSetHandler {
	return DeleteDhcpOptionSetHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Delete a DhcpOptionSet
// @Description Delete a DhcpOptionSet
// @Tags DhcpOptionSet
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param dhcpOptionSetId path string true "ID of DhcpOptionSet"
// @Success 204
// @Router /v2/org/{org}/nico/dhcp-option-set/{dhcpOptionSetId} [delete]
func (ddosh DeleteDhcpOptionSetHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("DhcpOptionSet", "Delete", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	orgTenant, apiErr := common.IsTenant(ctx, logger, ddosh.dbSession, org, dbUser, nil)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	dosID, err := uuid.Parse(c.Param("id"))
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid DHCP Option Set ID in URL", nil)
	}

	dosDAO := cdbm.NewDhcpOptionSetDAO(ddosh.dbSession)
	dos, err := dosDAO.GetByID(ctx, nil, dosID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not retrieve DHCP Option Set to delete", nil)
		}
		logger.Error().Err(err).Msg("error retrieving DHCP Option Set DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Could not retrieve DHCP Option Set to delete", nil)
	}

	if dos.TenantID != orgTenant.ID {
		logger.Warn().Msg("Tenant in DHCP Option Set does not belong to Tenant in org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant for DHCP Option Set in request does not match Tenant in org", nil)
	}

	stc, derr := ddosh.scp.GetClientByID(dos.SiteID)
	if derr != nil {
		logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	err = cdb.WithTx(ctx, ddosh.dbSession, func(tx *cdb.Tx) error {
		// Shares the per-set lock with updates so a delete can't interleave with an update of this set.
		derr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString("dhcp-option-set:"+dos.ID.String()), nil)
		if derr != nil {
			if errors.Is(derr, cdb.ErrXactAdvisoryLockFailed) {
				logger.Warn().Err(derr).Msg("concurrent DHCP Option Set change detected")
				return cutil.NewAPIError(http.StatusConflict, "Another request is changing this DHCP Option Set, retry the request", nil)
			}
			logger.Error().Err(derr).Msg("unable to acquire advisory lock for DHCP Option Set deletion")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete DHCP Option Set due to DB error", nil)
		}

		current, derr := dosDAO.GetByID(ctx, tx, dos.ID, nil)
		if derr != nil {
			if errors.Is(derr, cdb.ErrDoesNotExist) {
				return cutil.NewAPIError(http.StatusNotFound, "Could not retrieve DHCP Option Set to delete", nil)
			}
			logger.Error().Err(derr).Msg("error retrieving DHCP Option Set DB entity")
			return cutil.NewAPIError(http.StatusInternalServerError, "Could not retrieve DHCP Option Set to delete", nil)
		}

		logger.Info().Str("DHCP Option Set ID", current.ID.String()).Msg("deleting DHCP Option Set via Core proxy")

		// A retry joins a running call. After it closes, Temporal can start a new execution.
		workflowID := "core-grpc-DeleteDhcpOptionSet-" + current.ID.String()
		proxyErr := common.ExecuteCoreGRPCWithConflictPolicy(ctx, stc, corev1.Forge_DeleteDhcpOptionSet_FullMethodName,
			current.ToDeletionRequestProto(), nil, workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING, "")
		if proxyErr != nil {
			// A set the Site no longer knows about is already in the state the
			// caller asked for, so let the deletion proceed.
			if proxyErr.Code != http.StatusNotFound {
				logAPIError(logger, proxyErr, "failed to delete DHCP Option Set on Site")
				return cutil.NewAPIError(proxyErr.Code, proxyErr.Message, nil)
			}
			logger.Warn().Msg("DHCP Option Set not found on Site, treating as already deleted")
		}

		derr = dosDAO.Delete(ctx, tx, current.ID)
		if derr != nil {
			logger.Error().Err(derr).Msg("error deleting DHCP Option Set in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete DHCP Option Set, DB error", nil)
		}

		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to delete DHCP Option Set, DB transaction error")
	}

	logger.Info().Msg("finishing API handler")
	return c.NoContent(http.StatusNoContent)
}
