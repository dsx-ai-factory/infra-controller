// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strconv"

	"go.opentelemetry.io/otel/attribute"
	temporalClient "go.temporal.io/sdk/client"

	mapset "github.com/deckarep/golang-set/v2"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"

	"github.com/labstack/echo/v4"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cipam "github.com/NVIDIA/infra-controller/rest-api/ipam"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
)

// ~~~~~ Create Handler ~~~~~ //

// CreateIPBlockHandler is the API Handler for creating new IPBlock
type CreateIPBlockHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewCreateIPBlockHandler initializes and returns a new handler for creating IPBlock
func NewCreateIPBlockHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) CreateIPBlockHandler {
	return CreateIPBlockHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Create a IPBlock
// @Description Create a IPBlock
// @Tags IPBlock
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIIPBlockCreateRequest true "IPBlock creation request"
// @Success 201 {object} model.APIIPBlock
// @Router /v2/org/{org}/nico/ipblock [post]
func (cipbh CreateIPBlockHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("IPBlock", "Create", c, cipbh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to create IP Blocks
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIIPBlockCreateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating IP Block creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating IP Block creation request data", verr)
	}

	// Check that infrastructureProvider for org matches request
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, cipbh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve infrastructure provider", nil)
	}

	// Validate the site for which this IPBlock is being created
	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, cipbh.dbSession)
	if err != nil {
		logger.Warn().Str("siteId", apiRequest.SiteID).Err(err).Msg("error getting site from request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error retrieving Site in request", nil)
	}
	// verify site's infrastructure provider matches org's infrastructure provider
	if site.InfrastructureProviderID != ip.ID {
		logger.Warn().Msg("site infrastructure provider does not match org's infrastructure provider")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site Infrastructure Provider does not match Org", nil)
	}

	ipbDAO := cdbm.NewIPBlockDAO(cipbh.dbSession)

	var ipb *cdbm.IPBlock
	var ssd *cdbm.StatusDetail
	err = cdb.WithTx(ctx, cipbh.dbSession, func(tx *cdb.Tx) error {
		// Site Config prefix import and DatacenterOnly IP Block creation share
		// this lock; SitePrefix inventory reconciliation will use it too. Run
		// uniqueness checks after successful acquisition so a retried loser sees
		// the root that committed first.
		if apiRequest.RoutingType == cdbm.IPBlockRoutingTypeDatacenterOnly {
			derr := tx.TryAcquireAdvisoryLock(
				ctx,
				cdbm.SiteFabricIPBlockLockID(site.InfrastructureProviderID, site.ID),
				nil,
			)
			if derr != nil {
				if errors.Is(derr, cdb.ErrXactAdvisoryLockFailed) {
					logger.Warn().Err(derr).Msg("Site fabric IP Block lock is held by another writer")
					return cutil.NewAPIError(http.StatusConflict, "Site fabric IP Blocks are being updated; retry the request", nil)
				}
				logger.Error().Err(derr).Msg("failed to acquire Site fabric IP Block lock")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create IP Block", nil)
			}
		}

		// TODO consider serializing name uniqueness across all routing types.
		ipbs, total, derr := ipbDAO.GetAll(
			ctx,
			tx,
			cdbm.IPBlockFilterInput{
				SiteIDs:                   []uuid.UUID{site.ID},
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
				Names:                     []string{apiRequest.Name},
				ExcludeDerived:            true,
			},
			cdbp.PageInput{},
			nil,
		)
		if derr != nil {
			logger.Error().Err(derr).Msg("db error checking for name uniqueness of ip block")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create IPBlock due to db error", nil)
		}
		if total > 0 {
			logger.Warn().Str("providerId", ip.ID.String()).Str("name", apiRequest.Name).Msg("ip block with same name already exists")
			return cutil.NewAPIError(http.StatusConflict, fmt.Sprintf("IPBlock with name: %s for Site: %s already exists for provider", apiRequest.Name, apiRequest.SiteID), validation.Errors{
				"id": errors.New(ipbs[0].ID.String()),
			})
		}

		ipbs, total, derr = ipbDAO.GetAll(
			ctx,
			tx,
			cdbm.IPBlockFilterInput{
				SiteIDs:                   []uuid.UUID{site.ID},
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
				Prefixes:                  []string{apiRequest.Prefix},
				PrefixLengths:             []int{apiRequest.PrefixLength},
				RoutingTypes:              []string{apiRequest.RoutingType},
				ExcludeDerived:            true,
			},
			cdbp.PageInput{},
			nil,
		)
		if derr != nil {
			logger.Error().Err(derr).Msg("db error checking for prefix and prefixlength uniqueness of ip block")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create IPBlock due to db error", nil)
		}
		if total > 0 {
			logger.Warn().Str("providerId", ip.ID.String()).Str("prefix", apiRequest.Prefix).Int("prefix_length", apiRequest.PrefixLength).Msg("ip block with same prefix and prefix_length already exists")
			return cutil.NewAPIError(http.StatusConflict, fmt.Sprintf("IPBlock with prefix: %s and prefix_length: %d for Site: %s already exists for provider", apiRequest.Prefix, apiRequest.PrefixLength, apiRequest.SiteID), validation.Errors{
				"id": errors.New(ipbs[0].ID.String()),
			})
		}

		ipamStorage := ipam.NewIpamStorage(cipbh.dbSession.DB, tx.GetBunTx())
		// Create the prefix in IPAM
		prefix, derr := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, apiRequest.Prefix, apiRequest.PrefixLength, apiRequest.RoutingType, ip.ID.String(), site.ID.String())
		if derr != nil {
			logger.Warn().Err(derr).Msg("error creating ipam prefix")
			return cutil.NewAPIError(http.StatusConflict, fmt.Sprintf("Could not create IPAM entry for IPBlock. Details: %s", derr.Error()), nil)
		}
		logger.Info().Str("namespace", prefix.Namespace).Str("prefix", prefix.String()).Msg("created prefix in ipam")

		// Create the db record for IPBlock
		ipb, derr = ipbDAO.Create(
			ctx,
			tx,
			cdbm.IPBlockCreateInput{
				Name:                     apiRequest.Name,
				Description:              apiRequest.Description,
				SiteID:                   site.ID,
				InfrastructureProviderID: ip.ID,
				RoutingType:              apiRequest.RoutingType,
				Prefix:                   apiRequest.Prefix,
				PrefixLength:             apiRequest.PrefixLength,
				ProtocolVersion:          apiRequest.ProtocolVersion,
				Status:                   cdbm.IPBlockStatusReady,
				CreatedBy:                &dbUser.ID,
			},
		)
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating IPBlock in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create ip block", nil)
		}

		// Create a status detail record for the IPBlock
		sdDAO := cdbm.NewStatusDetailDAO(cipbh.dbSession)
		var serr error
		ssd, serr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: ipb.ID.String(), Status: *cutil.GetPtr(cdbm.IPBlockStatusReady), Message: cutil.GetPtr("IP Block is ready for use")})
		if serr != nil {
			logger.Error().Err(serr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for IPBlock", nil)
		}
		if ssd == nil {
			logger.Error().Msg("Status Detail DB entry not returned from Create")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to get new Status Detail for IPBlock", nil)
		}
		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to create IPBlock due to DB transaction error")
	}

	// Create response
	apiInstance := model.NewAPIIPBlock(ipb, []cdbm.StatusDetail{*ssd}, nil)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusCreated, apiInstance)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllIPBlockHandler is the API Handler for getting all IPBlocks
type GetAllIPBlockHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetAllIPBlockHandler initializes and returns a new handler for getting all IPBlocks
func NewGetAllIPBlockHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllIPBlockHandler {
	return GetAllIPBlockHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get all IPBlocks
// @Description Get all IPBlocks
// @Tags IPBlock
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param infrastructureProviderId query string false "Deprecated: Infrastructure Provider is now inferred from the org's membership"
// @Param tenantId query string false "Deprecated: Tenant is now inferred from the org's membership"
// @Param siteId query string true "ID of Site"
// @Param status query string false "Filter by status" e.g. 'Pending', 'Error'"
// @Param query query string false "Query input for full text search"
// @Param includeRelation query string false "Related entities to include in response e.g. 'InfrastructureProvider', 'Tenant', 'Site'"
// @Param includeUsageStats query boolean false "IPBlock usage stats to include in response
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APIIPBlock
// @Router /v2/org/{org}/nico/ipblock [get]
func (gaipbh GetAllIPBlockHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("IPBlock", "GetAll", c, gaipbh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate pagination request
	pageRequest := pagination.PageRequest{}
	err := c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate request attributes
	err = pageRequest.Validate(cdbm.IPBlockOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.IPBlockRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Check `includeUsageStats` in query
	includeUsageStats := false
	qius := c.QueryParam("includeUsageStats")
	if qius != "" {
		includeUsageStats, err = strconv.ParseBool(qius)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `includeUsageStats` query param", nil)
		}
	}

	provider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, gaipbh.dbSession, org, dbUser, true, nil)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Get and validate query params
	// Validate Site ID if provided
	var siteIDs []uuid.UUID

	stDAO := cdbm.NewSiteDAO(gaipbh.dbSession)

	if qSiteID := c.QueryParam("siteId"); qSiteID != "" {
		siteID, serr := uuid.Parse(qSiteID)
		if serr != nil {
			logger.Warn().Err(serr).Msg("error parsing siteId in query into uuid")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Site ID in query", nil)
		}

		_, serr = stDAO.GetByID(ctx, nil, siteID, nil, false)
		if serr != nil {
			if serr == cdb.ErrDoesNotExist {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in query param does not exist", nil)
			}

			logger.Error().Err(serr).Msg("error retrieving Site from DB by ID")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in query param, DB error", nil)
		}

		siteIDs = append(siteIDs, siteID)
	}

	// Get query text for full text search from query param
	searchQuery := common.GetSearchQuery(c)
	if searchQuery != nil {
		gaipbh.tracerSpan.SetAttribute(handlerSpan, attribute.String("query", *searchQuery), logger)
	}

	// Get status from query param
	var statuses []string

	statusQuery := c.QueryParam("status")
	if statusQuery != "" {
		gaipbh.tracerSpan.SetAttribute(handlerSpan, attribute.String("status", statusQuery), logger)
		_, ok := cdbm.IPBlockStatusMap[statusQuery]
		if !ok {
			logger.Warn().Msg(fmt.Sprintf("invalid value in status query: %v", statusQuery))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Status value in query", nil)
		}
		statuses = append(statuses, statusQuery)
	}

	// Retrieve IP Blocks from DB
	ipbDAO := cdbm.NewIPBlockDAO(gaipbh.dbSession)

	ipbIDs := mapset.NewSet[uuid.UUID]()

	if provider != nil {
		// Retrieve all IP Blocks from Provider perspective
		providerFilter := cdbm.IPBlockFilterInput{
			SiteIDs:     siteIDs,
			Statuses:    statuses,
			SearchQuery: searchQuery,
		}
		providerFilter.SiteFabric(provider.ID)
		ipbs, _, err := ipbDAO.GetAll(ctx, nil, providerFilter, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)

		if err != nil {
			logger.Error().Err(err).Msg("error getting IPBlocks from db")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve IP Blocks, DB error", nil)
		}

		for _, ipb := range ipbs {
			ipbIDs.Add(ipb.ID)
		}
	}

	if tenant != nil {
		// Retrieve all IP Blocks from Tenant perspective
		tenantFilter := cdbm.IPBlockFilterInput{
			SiteIDs:     siteIDs,
			Statuses:    statuses,
			SearchQuery: searchQuery,
		}
		tenantFilter.TenantAllocated(tenant.ID)
		ipbs, _, err := ipbDAO.GetAll(ctx, nil, tenantFilter, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)

		if err != nil {
			logger.Error().Err(err).Msg("error getting IPBlocks from db")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve IP Blocks, DB error", nil)
		}

		for _, ipb := range ipbs {
			ipbIDs.Add(ipb.ID)
		}
	}

	// Retrieve the combined list of IP Blocks from Provider and Tenant perspectives
	ipbs, total, err := ipbDAO.GetAll(ctx, nil, cdbm.IPBlockFilterInput{
		IPBlockIDs: ipbIDs.ToSlice(),
	}, cdbp.PageInput{
		Offset:  pageRequest.Offset,
		Limit:   pageRequest.Limit,
		OrderBy: pageRequest.OrderBy,
	}, qIncludeRelations)

	if err != nil {
		logger.Error().Err(err).Msg("error getting IPBlocks from db")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve IP Blocks, DB error", nil)
	}

	// Get IPAM usage stats
	pagedIpbIDs := []string{}

	ipamStorage := ipam.NewIpamStorage(gaipbh.dbSession.DB, nil)
	puipbMap := map[uuid.UUID]*cipam.Usage{}

	// Loop through IP Blocks to populated paged IDs and IPAM usage stats
	for _, ipb := range ipbs {
		pagedIpbIDs = append(pagedIpbIDs, ipb.ID.String())

		if includeUsageStats {
			prefixUsage, serr := ipam.GetIpamUsageForIPBlock(ctx, ipamStorage, &ipb)
			if serr != nil {
				logger.Error().Err(serr).Msg("error retrieving ipam usage stats details for IPBlock")
			} else {
				puipbMap[ipb.ID] = prefixUsage
			}
		}
	}

	// Get status details
	sdDAO := cdbm.NewStatusDetailDAO(gaipbh.dbSession)

	ssds, serr := sdDAO.GetRecentByEntityIDs(ctx, nil, pagedIpbIDs, common.RECENT_STATUS_DETAIL_COUNT)
	if serr != nil {
		logger.Warn().Err(serr).Msg("error retrieving Status Details for IP Blocks from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate status history for IP Blocks", nil)
	}
	ssdMap := map[string][]cdbm.StatusDetail{}
	for _, ssd := range ssds {
		cssd := ssd
		ssdMap[ssd.EntityID] = append(ssdMap[ssd.EntityID], cssd)
	}

	// Create response
	apiIpbs := []*model.APIIPBlock{}

	// get status details
	for _, ipb := range ipbs {
		cipb := ipb
		cipu, _ := puipbMap[ipb.ID]
		apiIpb := model.NewAPIIPBlock(&cipb, ssdMap[cipb.ID.String()], cipu)
		apiIpbs = append(apiIpbs, apiIpb)
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

	return c.JSON(http.StatusOK, apiIpbs)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllDerivedIPBlockHandler is the API Handler for getting details of derived IPBlocks from a parent IPBlock
type GetAllDerivedIPBlockHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetAllDerivedIPBlockHandler initializes and returns a new handler for getting derived IPBlocks
func NewGetAllDerivedIPBlockHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllDerivedIPBlockHandler {
	return GetAllDerivedIPBlockHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get all Derived IPBlocks
// @Description Get all Derived IPBlocks for a given parent IPBlock
// @Tags IPBlock
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of IPBlock"
// @Param status query string false "Filter by status" e.g. 'Pending', 'Error'"
// @Param query query string false "Query input for full text search"
// @Param includeRelation query string false "Related entities to include in response e.g. 'InfrastructureProvider', 'Tenant', 'Site'"
// @Success 200 {object} model.APIIPBlock
// @Router /v2/org/{org}/nico/ipblock/{id}/derived [get]
func (gadipbh GetAllDerivedIPBlockHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("IPBlock", "GetAllDerived", c, gadipbh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, Provider Admin/Viewer are allowed to retrieve all derived IP Blocks across Tenants
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole, auth.ProviderViewerRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Check that infrastructureProvider for org matches request
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, gadipbh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve infrastructure provider", nil)
	}

	// Validate pagination request
	pageRequest := pagination.PageRequest{}
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate request attributes
	err = pageRequest.Validate(cdbm.IPBlockOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.IPBlockRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Get ipBlock ID from URL param
	ipbStrID := c.Param("id")

	gadipbh.tracerSpan.SetAttribute(handlerSpan, attribute.String("ipblock_id", ipbStrID), logger)

	ipbID, err := uuid.Parse(ipbStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid IP Block ID in URL", nil)
	}

	// Get query text for full text search from query param
	searchQuery := common.GetSearchQuery(c)
	if searchQuery != nil {
		gadipbh.tracerSpan.SetAttribute(handlerSpan, attribute.String("query", *searchQuery), logger)
	}

	// Get status from query param
	var statuses []string

	statusQuery := c.QueryParam("status")
	if statusQuery != "" {
		gadipbh.tracerSpan.SetAttribute(handlerSpan, attribute.String("status", statusQuery), logger)
		_, ok := cdbm.IPBlockStatusMap[statusQuery]
		if !ok {
			logger.Warn().Msg(fmt.Sprintf("invalid value in status query: %v", statusQuery))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Status value in query", nil)
		}
		statuses = append(statuses, statusQuery)
	}

	ipbDAO := cdbm.NewIPBlockDAO(gadipbh.dbSession)

	// The requested parent must be a root belonging to this provider.
	parentFilter := cdbm.IPBlockFilterInput{}
	parentFilter.SiteFabric(ip.ID)
	ipb, err := ipbDAO.GetOne(ctx, nil, ipbID, parentFilter, nil)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find parent IPBlock with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving parent IPBlock DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve parent IP Blocks, error communicating with DB", nil)
	}

	// Get allocation constraints by resourcetype ID (parent IPBlock)
	acDAO := cdbm.NewAllocationConstraintDAO(gadipbh.dbSession)
	acs, _, err := acDAO.GetAll(ctx, nil, cdbm.AllocationConstraintFilterInput{
		ResourceType:    cutil.GetPtr(cdbm.AllocationResourceTypeIPBlock),
		ResourceTypeIDs: []uuid.UUID{ipb.ID},
	}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Allocation Constraints for parent IPBlock from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Allocation Constraints for parent IPBlock", nil)
	}

	childIPBlockIDs := []uuid.UUID{}
	for _, ac := range acs {
		if ac.DerivedResourceID != nil {
			childIPBlockIDs = append(childIPBlockIDs, *ac.DerivedResourceID)
		}
	}

	// Create response
	apiIpbs := []*model.APIIPBlock{}

	// Get all derived IPBlocks from IDs
	dipbs, total, err := ipbDAO.GetAll(
		ctx,
		nil,
		cdbm.IPBlockFilterInput{
			Statuses:    statuses,
			SearchQuery: searchQuery,
			IPBlockIDs:  childIPBlockIDs,
		},
		cdbp.PageInput{
			Offset:  pageRequest.Offset,
			Limit:   pageRequest.Limit,
			OrderBy: pageRequest.OrderBy,
		},
		qIncludeRelations,
	)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving derived IPBlocks from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve derived IPBlocks", nil)
	}

	// Get status details for derived IPBlocks
	sdEntityIDs := []string{}
	for _, dipb := range dipbs {
		sdEntityIDs = append(sdEntityIDs, dipb.ID.String())
	}

	sdDAO := cdbm.NewStatusDetailDAO(gadipbh.dbSession)
	dssds, err := sdDAO.GetRecentByEntityIDs(ctx, nil, sdEntityIDs, common.RECENT_STATUS_DETAIL_COUNT)
	if err != nil {
		logger.Warn().Err(err).Msg("error retrieving Status Details for derived IP Blocks from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate status history for derived IP Blocks", nil)
	}

	dssdMap := map[string][]cdbm.StatusDetail{}
	for _, dssd := range dssds {
		cssd := dssd
		dssdMap[dssd.EntityID] = append(dssdMap[dssd.EntityID], cssd)
	}

	// Prepare derived blocks details
	for _, ipb := range dipbs {
		cipb := ipb
		apiIpb := model.NewAPIIPBlock(&cipb, dssdMap[cipb.ID.String()], nil)
		apiIpbs = append(apiIpbs, apiIpb)
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

	return c.JSON(http.StatusOK, apiIpbs)
}

// ~~~~~ Get Handler ~~~~~ //

// GetIPBlockHandler is the API Handler for retrieving IPBlock
type GetIPBlockHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetIPBlockHandler initializes and returns a new handler to retrieve IPBlock
func NewGetIPBlockHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetIPBlockHandler {
	return GetIPBlockHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Retrieve the IPBlock
// @Description Retrieve the IPBlock
// @Tags IPBlock
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of IPBlock"
// @Param infrastructureProviderId query string false "Deprecated: Infrastructure Provider is now inferred from the org's membership"
// @Param tenantId query string false "Deprecated: Tenant is now inferred from the org's membership"
// @Param includeRelation query string false "Related entities to include in response e.g. 'InfrastructureProvider', 'Tenant', 'Site'"
// @Param includeUsageStats query boolean false "IPBlock usage stats to include in response
// @Success 200 {object} model.APIIPBlock
// @Router /v2/org/{org}/nico/ipblock/{id} [get]
func (gipbh GetIPBlockHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("IPBlock", "Get", c, gipbh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.IPBlockRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Get ipBlock ID from URL param
	ipbStrID := c.Param("id")

	gipbh.tracerSpan.SetAttribute(handlerSpan, attribute.String("ipblock_id", ipbStrID), logger)

	ipbID, err := uuid.Parse(ipbStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid IP Block ID in URL", nil)
	}

	// Check `includeUsageStats` in query
	includeUsageStats := false
	qius := c.QueryParam("includeUsageStats")
	if qius != "" {
		includeUsageStats, err = strconv.ParseBool(qius)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `includeUsageStats` query param", nil)
		}
	}

	provider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, gipbh.dbSession, org, dbUser, true, nil)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	ipbDAO := cdbm.NewIPBlockDAO(gipbh.dbSession)

	var ipb *cdbm.IPBlock
	if provider != nil {
		providerFilter := cdbm.IPBlockFilterInput{}
		providerFilter.ProviderVisible(provider.ID)
		ipb, err = ipbDAO.GetOne(ctx, nil, ipbID, providerFilter, qIncludeRelations)
		if err != nil && !errors.Is(err, cdb.ErrDoesNotExist) {
			logger.Error().Err(err).Msg("error retrieving IPBlock visible to provider from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve IP Block, DB error", nil)
		}
	}

	if ipb == nil && tenant != nil {
		tenantFilter := cdbm.IPBlockFilterInput{}
		tenantFilter.TenantAllocated(tenant.ID)
		ipb, err = ipbDAO.GetOne(ctx, nil, ipbID, tenantFilter, qIncludeRelations)
		if err != nil && !errors.Is(err, cdb.ErrDoesNotExist) {
			logger.Error().Err(err).Msg("error retrieving IPBlock visible to tenant from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve IP Block, DB error", nil)
		}
	}

	if ipb == nil {
		logger.Warn().Str("IP Block ID", ipbID.String()).Msg("IPBlock not visible to org")
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find IP Block with specified ID", nil)
	}

	// Get status details
	sdDAO := cdbm.NewStatusDetailDAO(gipbh.dbSession)

	ssds, err := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{ipb.ID.String()}, common.RECENT_STATUS_DETAIL_COUNT)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Status Details for IPBlock from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Status Details for IPBlock", nil)
	}

	// Get IPAM usage stats
	var puipb *cipam.Usage
	if includeUsageStats {
		// Get Usage stats from IPAM for the IPBlock
		ipamStorage := ipam.NewIpamStorage(gipbh.dbSession.DB, nil)
		puipb, err = ipam.GetIpamUsageForIPBlock(ctx, ipamStorage, ipb)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving ipam usage stats details for IPBlock")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Usage Stats Details for IPBlock", nil)
		}
	}

	// Create API response
	apiIPBlock := model.NewAPIIPBlock(ipb, ssds, puipb)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiIPBlock)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateIPBlockHandler is the API Handler for updating a IPBlock
type UpdateIPBlockHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewUpdateIPBlockHandler initializes and returns a new handler for updating IPBlock
func NewUpdateIPBlockHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) UpdateIPBlockHandler {
	return UpdateIPBlockHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Update an existing IPBlock
// @Description Update an existing IPBlock
// @Tags IPBlock
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of IPBlock"
// @Param message body model.APIIPBlockUpdateRequest true "IPBlock update request"
// @Success 200 {object} model.APIIPBlock
// @Router /v2/org/{org}/nico/ipblock/{id} [patch]
func (uipbh UpdateIPBlockHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("IPBlock", "Update", c, uipbh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to update IP Blocks
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get ipBlock ID from URL param
	ipbStrID := c.Param("id")

	uipbh.tracerSpan.SetAttribute(handlerSpan, attribute.String("ipblock_id", ipbStrID), logger)

	ipbID, err := uuid.Parse(ipbStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid IPBlock ID in URL", nil)
	}

	ipbDAO := cdbm.NewIPBlockDAO(uipbh.dbSession)

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIIPBlockUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating IP Block update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating IP Block update request data", verr)
	}

	// Check that the org's infrastructureProvider matches infrastructure provider in ipBlock
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, uipbh.dbSession, org)
	if err != nil {
		logger.Warn().Str("org", org).Err(err).Msg("infrastructureProvider does not exist for org")
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Error retrieving infrastructureProvider for org", nil)
	}

	providerFilter := cdbm.IPBlockFilterInput{}
	providerFilter.ProviderVisible(ip.ID)
	ipb, err := ipbDAO.GetOne(ctx, nil, ipbID, providerFilter, nil)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			logger.Warn().Str("IP Block ID", ipbID.String()).Msg("IPBlock not visible to provider")
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not retrieve IPBlock to update", nil)
		}
		logger.Error().Err(err).Msg("error retrieving provider IPBlock DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve IPBlock, DB error", nil)
	}

	var names []string
	if apiRequest.Name != nil {
		names = append(names, *apiRequest.Name)
	}

	// Check if an ipblock already exists for the provider with given name at the site
	if apiRequest.Name != nil && *apiRequest.Name != ipb.Name {
		ipbs, tot, serr := ipbDAO.GetAll(
			ctx,
			nil,
			cdbm.IPBlockFilterInput{
				SiteIDs:                   []uuid.UUID{ipb.SiteID},
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
				Names:                     names,
				ExcludeDerived:            true,
			},
			cdbp.PageInput{},
			nil,
		)
		if serr != nil {
			logger.Error().Err(serr).Msg("db error checking for name uniqueness of ip block")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to update IPBlock due to db error", nil)
		}
		if tot > 0 {
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Another IP Block with same name already exists for this Site and Provider", validation.Errors{
				"id": errors.New(ipbs[0].ID.String()),
			})
		}
	}

	// Update IPBlock in DB
	var ssds []cdbm.StatusDetail
	ipb, err = cdb.WithTxResult(ctx, uipbh.dbSession, func(tx *cdb.Tx) (*cdbm.IPBlock, error) {
		updated, derr := ipbDAO.Update(
			ctx,
			tx,
			cdbm.IPBlockUpdateInput{
				IPBlockID:   ipbID,
				Name:        apiRequest.Name,
				Description: apiRequest.Description,
			},
		)
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating IPBlock in DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to update IPBlock", nil)
		}

		sdDAO := cdbm.NewStatusDetailDAO(uipbh.dbSession)
		var sderr error
		ssds, _, sderr = sdDAO.GetAll(ctx, tx, cdbm.StatusDetailFilterInput{EntityIDs: []string{updated.ID.String()}}, cdbp.PageInput{Limit: cutil.GetPtr(pagination.MaxPageSize)})
		if sderr != nil {
			logger.Error().Err(sderr).Msg("error retrieving Status Details for IPBlock from DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Status Details for IPBlock", nil)
		}
		return updated, nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to update IPBlock due to DB transaction error")
	}

	// Create response
	apiInstance := model.NewAPIIPBlock(ipb, ssds, nil)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiInstance)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteIPBlockHandler is the API Handler for deleting a IPBlock
type DeleteIPBlockHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewDeleteIPBlockHandler initializes and returns a new handler for deleting IPBlock
func NewDeleteIPBlockHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) DeleteIPBlockHandler {
	return DeleteIPBlockHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Delete an existing IPBlock
// @Description Delete an existing IPBlock
// @Tags IPBlock
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of IPBlock"
// @Success 202
// @Router /v2/org/{org}/nico/ipblock/{id} [delete]
func (dipbh DeleteIPBlockHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("IPBlock", "Delete", c, dipbh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for user in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to delete IP Blocks
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get ipBlock ID from URL param
	ipbStrID := c.Param("id")

	dipbh.tracerSpan.SetAttribute(handlerSpan, attribute.String("ipblock_id", ipbStrID), logger)

	ipbID, err := uuid.Parse(ipbStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid IP Block ID in URL", nil)
	}

	logger.Info().Str("IP Block ID", ipbStrID).Msg("deleting IP Block")

	ipbDAO := cdbm.NewIPBlockDAO(dipbh.dbSession)

	// Check that the org's infrastructureProvider matches infrastructureProvider in IPBlock
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, dipbh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error getting Infrastructure Provider for Org", nil)
	}
	siteFabricFilter := cdbm.IPBlockFilterInput{}
	siteFabricFilter.SiteFabric(ip.ID)
	ipb, err := ipbDAO.GetOne(ctx, nil, ipbID, siteFabricFilter, nil)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			logger.Warn().Str("IP Block ID", ipbID.String()).Msg("IPBlock not visible to provider")
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Specified IP Block does not exist, or has been deleted", nil)
		}
		logger.Error().Str("IP Block ID", ipbID.String()).Err(err).Msg("error retrieving provider IP Block DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve IPBlock, DB error", nil)
	}
	const coreLinkedDeleteMessage = "This IP Block is linked to an OperatorManaged SitePrefix; remove the corresponding prefix from Core's site_fabric_prefixes configuration instead"
	if ipb.SitePrefixID != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, coreLinkedDeleteMessage, nil)
	}

	err = cdb.WithTx(ctx, dipbh.dbSession, func(tx *cdb.Tx) error {
		lockedIPBlock, derr := ipbDAO.GetByIDForUpdate(ctx, tx, ipbID)
		if derr != nil {
			if errors.Is(derr, cdb.ErrDoesNotExist) {
				return cutil.NewAPIError(http.StatusNotFound, "Specified IP Block does not exist, or has been deleted", nil)
			}
			logger.Error().Err(derr).Msg("error locking IP Block in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Error locking IP Block, DB error", nil)
		}
		if lockedIPBlock.SitePrefixID != nil {
			return cutil.NewAPIError(http.StatusConflict, coreLinkedDeleteMessage, nil)
		}

		// Check for allocations while holding the lock on a DatacenterOnly Site
		// fabric root that can receive a Core link. A child that commits first is
		// visible here; a later child must wait for deletion.
		acDAO := cdbm.NewAllocationConstraintDAO(dipbh.dbSession)
		_, acCount, derr := acDAO.GetAll(ctx, tx, cdbm.AllocationConstraintFilterInput{
			ResourceType:    cutil.GetPtr(cdbm.AllocationResourceTypeIPBlock),
			ResourceTypeIDs: []uuid.UUID{lockedIPBlock.ID},
		}, cdbp.PageInput{}, nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("error getting allocation constraints")
			return cutil.NewAPIError(http.StatusInternalServerError, "Error retrieving Allocations for IP Block", nil)
		}
		if acCount > 0 {
			logger.Warn().Msg("allocation constraints exist for ipBlock")
			return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("%v Allocations exist for IP Block, unable to delete", acCount), nil)
		}

		// Delete IPBlock in DB
		derr = ipbDAO.Delete(ctx, tx, ipbID)
		if derr != nil {
			logger.Error().Err(derr).Msg("error deleting IP Block in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Error deleting IP Block, DB error", nil)
		}

		// delete IPAM entry for this ipBlock
		ipamStorage := ipam.NewIpamStorage(dipbh.dbSession.DB, tx.GetBunTx())
		derr = ipam.DeleteIpamEntryForIPBlock(ctx, ipamStorage, lockedIPBlock.Prefix, lockedIPBlock.PrefixLength, lockedIPBlock.RoutingType, lockedIPBlock.InfrastructureProviderID.String(), lockedIPBlock.SiteID.String())
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to delete IPAM record for IP Block")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Could not delete IPAM entry for IP Block. Details: %s", derr.Error()), nil)
		}
		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to delete IPBlock due to DB transaction error")
	}

	// Create response
	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusAccepted, model.NewAPIDeletionAcceptedResponse())
}
