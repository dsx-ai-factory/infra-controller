// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"

	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"

	"go.opentelemetry.io/otel/attribute"
	temporalClient "go.temporal.io/sdk/client"

	mapset "github.com/deckarep/golang-set/v2"
	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	common "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
)

// ~~~~~ Create Handler ~~~~~ //

// CreateTenantAccountHandler is the API Handler for creating new TenantAccount
type CreateTenantAccountHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewCreateTenantAccountHandler initializes and returns a new handler for creating TenantAccount
func NewCreateTenantAccountHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) CreateTenantAccountHandler {
	return CreateTenantAccountHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Create a TenantAccount
// @Description Create a TenantAccount
// @Tags tenantaccount
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APITenantAccountCreateRequest true "TenantAccount creation request"
// @Success 201 {object} model.APITenantAccount
// @Router /v2/org/{org}/nico/tenant/account [post]
func (ctah CreateTenantAccountHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TenantAccount", "Create", c)
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

	// Validate role, only Provider Admins are allowed to create TenantAccounts
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Validate request
	// Bind request data to API model
	apiRequest := model.APITenantAccountCreateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Tenant Account creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Tenant Account creation request data", verr)
	}

	// Check that infrastructureProvider for org matches request
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, ctah.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for current org", nil)
	}

	// Deprecated: infrastructureProviderId in request body. Infer from org when not provided.
	if apiRequest.InfrastructureProviderID == "" {
		apiRequest.InfrastructureProviderID = ip.ID.String()
	}

	if ip.ID.String() != apiRequest.InfrastructureProviderID {
		logger.Warn().Err(err).Msg("infrastructure provider in request does not belong to org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Infrastructure ProviderId in request", nil)
	}

	// Validate the Tenant for which this TenantAccount is being created
	// Atmost 1 tenant account can be created per (Infrastructure Provider, Tenant)
	tnDAO := cdbm.NewTenantDAO(ctah.dbSession)
	taDAO := cdbm.NewTenantAccountDAO(ctah.dbSession)

	// Request data validation guarantees that either TenantID or TenantOrg is specified
	var tenantID *uuid.UUID
	var tenantOrg *string

	if apiRequest.TenantID != nil {
		id, serr := uuid.Parse(*apiRequest.TenantID)
		if serr != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Tenant ID specified in request", nil)
		}
		tenant, serr := tnDAO.GetByID(ctx, nil, id, nil)
		if serr != nil {
			logger.Warn().Err(serr).Msg("error retrieving tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Tenant specified in request", nil)
		}
		tenantID = &tenant.ID
		tenantOrg = &tenant.Org
	} else {
		tenantOrg = apiRequest.TenantOrg
		tenants, _, serr := tnDAO.GetAll(ctx, nil, cdbm.TenantFilterInput{Orgs: []string{*tenantOrg}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if serr != nil {
			logger.Warn().Err(err).Msg("error retrieving tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Tenant specified in request", nil)
		}

		if len(tenants) > 1 {
			logger.Warn().Err(err).Msg("multiple tenants found for org")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Multiple tenants found for org", nil)
		} else if len(tenants) > 0 {
			tenantID = &tenants[0].ID
		}
	}

	// NOTE: At this point tenantID may be nil, if the Tenant entity does not exist in the DB yet
	var tas []cdbm.TenantAccount
	var terr error
	tas, _, terr = taDAO.GetAll(ctx, nil, cdbm.TenantAccountFilterInput{
		InfrastructureProviderID: &ip.ID,
		TenantOrgs:               []string{*tenantOrg},
	}, cdbp.PageInput{}, nil)
	if terr != nil {
		logger.Error().Err(terr).Msg("error retrieving Tenant Accounts")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant Accounts", nil)
	}
	if len(tas) > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant Account between Infrastructure Provider and Tenant already exists", nil)
	}

	// Generate a unique account number
	accountNumber := common.GenerateAccountNumber()

	sdDAO := cdbm.NewStatusDetailDAO(ctah.dbSession)

	var ta *cdbm.TenantAccount
	var ssd *cdbm.StatusDetail

	err = cdb.WithTx(ctx, ctah.dbSession, func(tx *cdb.Tx) error {
		var derr error
		ta, derr = taDAO.Create(ctx, tx, cdbm.TenantAccountCreateInput{
			AccountNumber:             accountNumber,
			TenantID:                  tenantID,
			TenantOrg:                 *tenantOrg,
			InfrastructureProviderID:  ip.ID,
			InfrastructureProviderOrg: ip.Org,
			Status:                    cdbm.TenantAccountStatusInvited,
			CreatedBy:                 dbUser.ID,
		})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating TenantAccount in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create tenant account", nil)
		}

		// Create a status detail record for the tenantaccount
		ssd, derr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: ta.ID.String(), Status: *cutil.GetPtr(cdbm.TenantAccountStatusInvited), Message: cutil.GetPtr("received tenant account creation request, pending accept")})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for TenantAccount", nil)
		}
		if ssd == nil {
			logger.Error().Msg("Status Detail DB entry not returned from Create")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to get new Status Detail for TenantAccount", nil)
		}

		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to create Tenant Account, DB transaction error")
	}

	// Create response
	apiInstance := model.NewAPITenantAccount(ta, []cdbm.StatusDetail{*ssd}, 0, nil)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusCreated, apiInstance)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllTenantAccountHandler is the API Handler for getting all TenantAccounts
type GetAllTenantAccountHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetAllTenantAccountHandler initializes and returns a new handler for getting all TenantAccounts
func NewGetAllTenantAccountHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllTenantAccountHandler {
	return GetAllTenantAccountHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get all TenantAccounts
// @Description Get all TenantAccounts
// @Tags tenantaccount
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param infrastructureProviderId query string false "Deprecated: ID of Infrastructure Provider"
// @Param tenantId query string false "Filter TenantAccounts by Tenant ID (Provider role only; for Tenant role the tenant is inferred from org membership and this param is ignored)"
// @Param status query string false "Query input for status"
// @Param query query string false "Search query string"
// @Param includeRelation query string false "Related entities to include in response e.g. 'InfrastructureProvider', 'Tenant'"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APITenantAccount
// @Router /v2/org/{org}/nico/tenant/account [get]
func (gatah GetAllTenantAccountHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TenantAccount", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate paginantion request
	pageRequest := pagination.PageRequest{}
	err := c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate request attributes
	err = pageRequest.Validate(cdbm.TenantAccountOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.TenantAccountRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Get status from query param
	var statuses []string

	statusQuery := c.QueryParam("status")
	if statusQuery != "" {
		cotel.SetAttribute(handlerSpan, attribute.String("status", statusQuery))
		_, sok := cdbm.TenantAccountStatusMap[statusQuery]
		if !sok {
			logger.Warn().Msg(fmt.Sprintf("invalid value in status query: %v", statusQuery))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Status value in query", nil)
		}
		statuses = []string{statusQuery}
	}

	searchQuery := common.GetSearchQuery(c)
	if searchQuery != nil {
		cotel.SetAttribute(handlerSpan, attribute.String("query", *searchQuery))
	}

	// Optional Provider-side tenantId narrowing filter. The Tenant branch
	// ignores this and always pins to the caller's own tenant.
	var filterTenantIDs []uuid.UUID
	tenantIdQuery := c.QueryParam("tenantId")
	if tenantIdQuery != "" {
		cotel.SetAttribute(handlerSpan, attribute.String("tenantId", tenantIdQuery))
		id, serr := uuid.Parse(tenantIdQuery)
		if serr != nil {
			logger.Warn().Err(serr).Msg("error parsing tenantId in query into uuid")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid Tenant ID: %s in query", tenantIdQuery), nil)
		}
		filterTenantIDs = []uuid.UUID{id}
	}

	provider, tenant, apiErr := common.IsProviderOrTenant(ctx, logger, gatah.dbSession, org, dbUser, true, nil)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	taDAO := cdbm.NewTenantAccountDAO(gatah.dbSession)

	// default append `TenantContact`
	qIncludeRelations = append(qIncludeRelations, "TenantContact")

	sharedFilter := cdbm.TenantAccountFilterInput{
		Statuses:    statuses,
		SearchQuery: searchQuery,
	}
	mergedTenantAccountIDs := mapset.NewSet[uuid.UUID]()
	totalLimitPage := cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}

	if provider != nil {
		providerFilter := sharedFilter
		providerFilter.InfrastructureProviderID = &provider.ID
		providerFilter.TenantIDs = filterTenantIDs
		tenantAccountsFromProviderPerspective, _, err := taDAO.GetAll(ctx, nil, providerFilter, totalLimitPage, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error getting TenantAccounts from Provider perspective")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve TenantAccounts, DB error", nil)
		}
		for _, tenantAccount := range tenantAccountsFromProviderPerspective {
			mergedTenantAccountIDs.Add(tenantAccount.ID)
		}
	}

	if tenant != nil {
		tenantFilter := sharedFilter
		tenantFilter.TenantIDs = []uuid.UUID{tenant.ID}
		tenantAccountsFromTenantPerspective, _, err := taDAO.GetAll(ctx, nil, tenantFilter, totalLimitPage, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error getting TenantAccounts from Tenant perspective")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve TenantAccounts, DB error", nil)
		}
		for _, tenantAccount := range tenantAccountsFromTenantPerspective {
			mergedTenantAccountIDs.Add(tenantAccount.ID)
		}
	}

	tas, total, err := taDAO.GetAll(ctx, nil, cdbm.TenantAccountFilterInput{
		TenantAccountIDs: mergedTenantAccountIDs.ToSlice(),
	}, pageRequest.ConvertToDB(), qIncludeRelations)
	if err != nil {
		logger.Error().Err(err).Msg("error getting TenantAccounts from db")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve TenantAccounts", nil)
	}

	// Get status details
	sdDAO := cdbm.NewStatusDetailDAO(gatah.dbSession)

	sdEntityIDs := []string{}
	for _, ta := range tas {
		sdEntityIDs = append(sdEntityIDs, ta.ID.String())
	}
	ssds, serr := sdDAO.GetRecentByEntityIDs(ctx, nil, sdEntityIDs, common.RECENT_STATUS_DETAIL_COUNT)
	if serr != nil {
		logger.Warn().Err(serr).Msg("error retrieving Status Details for Tenant Accounts from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate status history for Tenant Accounts", nil)
	}
	ssdMap := map[string][]cdbm.StatusDetail{}
	for _, ssd := range ssds {
		cssd := ssd
		ssdMap[ssd.EntityID] = append(ssdMap[ssd.EntityID], cssd)
	}

	apiTas := []*model.APITenantAccount{}
	aDAO := cdbm.NewAllocationDAO(gatah.dbSession)

	tenantIDsByProvider := map[uuid.UUID]mapset.Set[uuid.UUID]{}
	for _, ta := range tas {
		if ta.TenantID == nil {
			continue
		}
		providerTenantIDs, ok := tenantIDsByProvider[ta.InfrastructureProviderID]
		if !ok {
			providerTenantIDs = mapset.NewSet[uuid.UUID]()
			tenantIDsByProvider[ta.InfrastructureProviderID] = providerTenantIDs
		}
		providerTenantIDs.Add(*ta.TenantID)
	}

	allocationCountByProviderAndTenant := map[uuid.UUID]map[uuid.UUID]int{}
	if len(tenantIDsByProvider) > 0 {
		allProviderIDs := make([]uuid.UUID, 0, len(tenantIDsByProvider))
		allTenantIDs := mapset.NewSet[uuid.UUID]()
		for providerID, providerTenantIDs := range tenantIDsByProvider {
			allProviderIDs = append(allProviderIDs, providerID)
			allTenantIDs = allTenantIDs.Union(providerTenantIDs)
		}

		allocationPage := cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}
		allocations, _, aerr := aDAO.GetAll(ctx, nil, cdbm.AllocationFilterInput{
			InfrastructureProviderIDs: allProviderIDs,
			TenantIDs:                 allTenantIDs.ToSlice(),
		}, allocationPage, nil)
		if aerr != nil {
			logger.Error().Err(aerr).Msg("error retrieving allocations for Tenant Accounts from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Allocations to determine total Allocation count for Tenants", nil)
		}
		for _, allocation := range allocations {
			tenantAllocationCounts, ok := allocationCountByProviderAndTenant[allocation.InfrastructureProviderID]
			if !ok {
				tenantAllocationCounts = map[uuid.UUID]int{}
				allocationCountByProviderAndTenant[allocation.InfrastructureProviderID] = tenantAllocationCounts
			}
			tenantAllocationCounts[allocation.TenantID]++
		}
	}

	// Batch-load TenantSite rows (with Site relation) for every Tenant across the
	// returned accounts so per-Site capability overrides can be resolved without
	// a per-account query. Rows are grouped by TenantID; NewAPITenantAccount
	// further narrows them to the account's provider.
	tenantSitesByTenant := map[uuid.UUID][]cdbm.TenantSite{}
	tenantIDSet := mapset.NewSet[uuid.UUID]()
	for _, ta := range tas {
		if ta.TenantID != nil {
			tenantIDSet.Add(*ta.TenantID)
		}
	}
	if tenantIDSet.Cardinality() > 0 {
		tsDAO := cdbm.NewTenantSiteDAO(gatah.dbSession)
		tenantSites, _, tserr := tsDAO.GetAll(ctx, nil, cdbm.TenantSiteFilterInput{
			TenantIDs: tenantIDSet.ToSlice(),
		}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, []string{"Site"})
		if tserr != nil {
			logger.Error().Err(tserr).Msg("error retrieving Tenant Sites for Tenant Accounts from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant Sites for Tenant Accounts", nil)
		}
		for _, ts := range tenantSites {
			cts := ts
			tenantSitesByTenant[ts.TenantID] = append(tenantSitesByTenant[ts.TenantID], cts)
		}
	}

	for _, ta := range tas {
		tmpTa := ta
		allocationCount := 0
		var tenantSites []cdbm.TenantSite
		if tmpTa.TenantID != nil {
			allocationCount = allocationCountByProviderAndTenant[tmpTa.InfrastructureProviderID][*tmpTa.TenantID]
			tenantSites = tenantSitesByTenant[*tmpTa.TenantID]
		}
		apiTa := model.NewAPITenantAccount(&tmpTa, ssdMap[ta.ID.String()], allocationCount, tenantSites)
		apiTas = append(apiTas, apiTa)
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

	return c.JSON(http.StatusOK, apiTas)
}

// ~~~~~ Get Current Handler ~~~~~ //

// GetTenantAccountHandler is the API Handler for retrieving TenantAccount
type GetTenantAccountHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetTenantAccountHandler initializes and returns a new handler to retrieve TenantAccount
func NewGetTenantAccountHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetTenantAccountHandler {
	return GetTenantAccountHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Retrieve the TenantAccount
// @Description Retrieve the TenantAccount
// @Tags tenantaccount
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Tenant Account"
// @Param infrastructureProviderId query string false "Deprecated: ID of Infrastructure Provider"
// @Param tenantId query string false "Deprecated: ID of Tenant"
// @Param includeRelation query string false "Related entities to include in response e.g. 'InfrastructureProvider', 'Tenant'"
// @Success 200 {object} model.APITenantAccount
// @Router /v2/org/{org}/nico/tenant/account/{id} [get]
func (gtah GetTenantAccountHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TenantAccount", "Get", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Get tenant account ID from URL param
	taStrID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("tenantaccount_id", taStrID))

	taID, err := uuid.Parse(taStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Tenant Account ID in URL", nil)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errStr := common.GetAndValidateQueryRelations(qParams, cdbm.TenantAccountRelatedEntities)
	if errStr != "" {
		logger.Warn().Msg(errStr)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errStr, nil)
	}

	// Check that TenantAccount exists
	taDAO := cdbm.NewTenantAccountDAO(gtah.dbSession)

	// default append `TenantContact`
	qIncludeRelations = append(qIncludeRelations, "TenantContact")

	ta, err := taDAO.GetByID(ctx, nil, taID, qIncludeRelations)
	if err != nil {
		logger.Warn().Err(err).Msg("error retrieving TenantAccount DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not retrieve Tenant Account to update", nil)
	}

	provider, tenant, apiErr := common.IsProviderOrTenant(ctx, logger, gtah.dbSession, org, dbUser, true, nil)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	authorized := (provider != nil && ta.InfrastructureProviderID == provider.ID) ||
		(tenant != nil && ta.TenantID != nil && *ta.TenantID == tenant.ID)
	if !authorized {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant Account is not associated with org", nil)
	}

	aDAO := cdbm.NewAllocationDAO(gtah.dbSession)
	total := 0
	if ta.TenantID != nil {
		cnt, cerr := aDAO.GetCount(ctx, nil, cdbm.AllocationFilterInput{
			InfrastructureProviderIDs: []uuid.UUID{ta.InfrastructureProviderID},
			TenantIDs:                 []uuid.UUID{*ta.TenantID},
		})
		if cerr != nil {
			logger.Error().Err(cerr).Msg("error retrieving allocation count for Tenant Account from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Allocations to determine total allocation for tenant account", nil)
		}
		total = cnt
	}

	sdDAO := cdbm.NewStatusDetailDAO(gtah.dbSession)
	ssds, err := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{ta.ID.String()}, common.RECENT_STATUS_DETAIL_COUNT)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Status Details for TenantAccount from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Status Details for TenantAccount", nil)
	}

	var tenantSites []cdbm.TenantSite
	if ta != nil && ta.TenantID != nil {
		tsDAO := cdbm.NewTenantSiteDAO(gtah.dbSession)
		tenantSites, _, err = tsDAO.GetAll(ctx, nil, cdbm.TenantSiteFilterInput{
			TenantIDs: []uuid.UUID{*ta.TenantID},
		}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, []string{"Site"})
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Tenant Sites for Tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant Sites for Tenant", nil)
		}
	}

	// Create response
	apiTenantAccount := model.NewAPITenantAccount(ta, ssds, total, tenantSites)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiTenantAccount)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateTenantAccountHandler is the API Handler for updating a TenantAccount
type UpdateTenantAccountHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewUpdateTenantAccountHandler initializes and returns a new handler for updating Tenant
func NewUpdateTenantAccountHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) UpdateTenantAccountHandler {
	return UpdateTenantAccountHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Update an existing TenantAccount
// @Description Update an existing TenantAccount
// @Tags tenantaccount
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Tenant Account"
// @Param message body model.APITenantAccountUpdateRequest true "TenantAccount update request"
// @Success 200 {object} model.APITenantAccount
// @Router /v2/org/{org}/nico/tenant/account/{id} [patch]
func (utah UpdateTenantAccountHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TenantAccount", "Update", c)
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

	isProviderAdmin := auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	isTenantAdmin := auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !isProviderAdmin && !isTenantAdmin {
		logger.Warn().Msg("user does not have Provider Admin or Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin or Tenant Admin role with org", nil)
	}

	// Get tenant account ID from URL param
	taStrID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("tenantaccount_id", taStrID))

	taID, err := uuid.Parse(taStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Tenant Account ID in URL", nil)
	}

	// Validate request
	apiRequest := model.APITenantAccountUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Tenant Account update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Tenant Account update request data", verr)
	}

	hasSiteCapabilities := apiRequest.HasSiteCapabilities()
	hasTenantContact := apiRequest.TenantContactID != nil
	if hasSiteCapabilities && hasTenantContact {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Request cannot include both tenantContactId and siteCapabilities", nil)
	}

	if hasSiteCapabilities {
		if !isProviderAdmin {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
		}
		if verr = apiRequest.SiteCapabilities.Validate(); verr != nil {
			logger.Warn().Err(verr).Msg("error validating Tenant Account siteCapabilities")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Tenant Account siteCapabilities", verr)
		}
		return utah.handleProviderSiteCapabilitiesUpdate(c, ctx, logger, org, dbUser, taID, apiRequest)
	}

	if !isTenantAdmin {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	return utah.handleTenantInviteAcceptance(c, ctx, logger, org, dbUser, taID, apiRequest)
}

func (utah UpdateTenantAccountHandler) handleTenantInviteAcceptance(c echo.Context, ctx context.Context, logger zerolog.Logger, org string, dbUser *cdbm.User, taID uuid.UUID, apiRequest model.APITenantAccountUpdateRequest) error {
	taDAO := cdbm.NewTenantAccountDAO(utah.dbSession)

	ta, err := taDAO.GetByID(ctx, nil, taID, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("error retrieving TenantAccount DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not retrieve TenantAccount to update", nil)
	}

	tn, err := common.GetTenantForOrg(ctx, nil, utah.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("tenant does not exist for org")
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Org does not have tenant", nil)
	}

	if ta.TenantID == nil || *ta.TenantID != tn.ID {
		logger.Warn().Msg("tenant in tenant account does not match tenant in org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest,
			"Tenant in org does not match tenant in TenantAccount", nil)
	}

	if apiRequest.TenantContactID != nil {
		tnContactID, err1 := uuid.Parse(*apiRequest.TenantContactID)
		if err1 != nil {
			logger.Warn().Err(err1).Msg("error parsing tenantContactId in request into uuid")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Tenant Contact ID in request", nil)
		}
		if tnContactID != dbUser.ID {
			logger.Warn().Msg("tenant contact id in request must be the requesting user")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant Contact ID in request must match requesting user", nil)
		}
	}

	if ta.Status != cdbm.TenantAccountStatusInvited {
		logger.Warn().Msg("tenant account status is not Invited")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant Account status is not Invited", nil)
	}

	// Values needed after the transaction closure
	var uta *cdbm.TenantAccount
	var ssds []cdbm.StatusDetail
	// Handle database updates -- both tenant account and status detail
	err = cdb.WithTx(ctx, utah.dbSession, func(tx *cdb.Tx) error {
		lockKey := fmt.Sprintf("tenant-account-invite-%s", taID.String())
		derr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(lockKey), nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("unable to acquire advisory lock for TenantAccount invite acceptance")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update TenantAccount", nil)
		}

		lockedTA, derr := taDAO.GetByID(ctx, tx, taID, nil)
		if derr != nil {
			logger.Warn().Err(derr).Msg("error retrieving TenantAccount DB entity within transaction")
			return cutil.NewAPIError(http.StatusNotFound, "Could not retrieve TenantAccount to update", nil)
		}
		if lockedTA.TenantID == nil || *lockedTA.TenantID != tn.ID {
			logger.Warn().Msg("tenant in tenant account does not match tenant in org")
			return cutil.NewAPIError(http.StatusBadRequest,
				"Tenant in org does not match tenant in TenantAccount", nil)
		}
		if lockedTA.Status != cdbm.TenantAccountStatusInvited {
			logger.Warn().Msg("tenant account status is not Invited")
			return cutil.NewAPIError(http.StatusBadRequest, "Tenant Account status is not Invited", nil)
		}

		uta, derr = taDAO.Update(ctx, tx, cdbm.TenantAccountUpdateInput{
			TenantAccountID: taID,
			TenantContactID: cutil.GetPtr(dbUser.ID),
			Status:          cutil.GetPtr(cdbm.TenantAccountStatusReady),
		})
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating TenantAccount in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update TenantAccount", nil)
		}
		logger.Info().Msg("updated TenantAccount in DB")

		sdDAO := cdbm.NewStatusDetailDAO(utah.dbSession)
		_, derr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: uta.ID.String(), Status: *cutil.GetPtr(cdbm.TenantAccountStatusReady), Message: cutil.GetPtr("received tenant account update request, ready")})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail for TenantAccount")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for TenantAccount", nil)
		}

		// Get status details for the response
		ssds, _, derr = sdDAO.GetAll(ctx, tx, cdbm.StatusDetailFilterInput{EntityIDs: []string{uta.ID.String()}}, cdbp.PageInput{Limit: cutil.GetPtr(pagination.MaxPageSize)})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Status Details for TenantAccount from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Status Details for TenantAccount", nil)
		}
		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to update Tenant Account, DB transaction error")
	}

	allocationCount := 0
	if uta.TenantID != nil {
		aDAO := cdbm.NewAllocationDAO(utah.dbSession)
		cnt, cerr := aDAO.GetCount(ctx, nil, cdbm.AllocationFilterInput{
			InfrastructureProviderIDs: []uuid.UUID{uta.InfrastructureProviderID},
			TenantIDs:                 []uuid.UUID{*uta.TenantID},
		})
		if cerr != nil {
			logger.Error().Err(cerr).Msg("error retrieving allocation count for updated Tenant Account")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve allocation count for Tenant Account", nil)
		}
		allocationCount = cnt
	}

	// Load TenantSite overrides so the response reflects any pre-acceptance
	// per-site capability configuration, matching the provider-side update flow.
	var tenantSites []cdbm.TenantSite
	if uta.TenantID != nil {
		tsDAO := cdbm.NewTenantSiteDAO(utah.dbSession)
		tenantSites, _, err = tsDAO.GetAll(ctx, nil, cdbm.TenantSiteFilterInput{
			TenantIDs: []uuid.UUID{*uta.TenantID},
		}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, []string{"Site"})
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Tenant Sites for updated Tenant Account")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant Sites for Tenant Account", nil)
		}
	}

	apiInstance := model.NewAPITenantAccount(uta, ssds, allocationCount, tenantSites)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiInstance)
}

func (utah UpdateTenantAccountHandler) handleProviderSiteCapabilitiesUpdate(c echo.Context, ctx context.Context, logger zerolog.Logger, org string, dbUser *cdbm.User, taID uuid.UUID, apiRequest model.APITenantAccountUpdateRequest) error {
	taDAO := cdbm.NewTenantAccountDAO(utah.dbSession)

	ta, err := taDAO.GetByID(ctx, nil, taID, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("error retrieving TenantAccount DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not retrieve TenantAccount to update", nil)
	}

	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, utah.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for current org", nil)
	}
	if ip.ID != ta.InfrastructureProviderID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant Account is not associated with org", nil)
	}
	if ta.TenantID == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant Account does not have an associated Tenant", nil)
	}

	err = cdb.WithTx(ctx, utah.dbSession, func(tx *cdb.Tx) error {
		lockKey := fmt.Sprintf("tenant-account-capabilities-%s-%s", ta.TenantID.String(), ta.InfrastructureProviderID.String())
		derr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(lockKey), nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("unable to acquire advisory lock for TenantAccount siteCapabilities update")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Tenant Account capabilities", nil)
		}

		// Re-read the TenantAccount under the advisory lock so the ownership
		// checks and subsequent writes operate on the same transactional
		// snapshot rather than the pre-transaction read used to derive the
		// lock key.
		ta, derr = taDAO.GetByID(ctx, tx, taID, nil)
		if derr != nil {
			logger.Warn().Err(derr).Msg("error retrieving TenantAccount DB entity within transaction")
			return cutil.NewAPIError(http.StatusNotFound, "Could not retrieve TenantAccount to update", nil)
		}
		if ip.ID != ta.InfrastructureProviderID {
			return cutil.NewAPIError(http.StatusForbidden, "Tenant Account is not associated with org", nil)
		}
		if ta.TenantID == nil {
			return cutil.NewAPIError(http.StatusBadRequest, "Tenant Account does not have an associated Tenant", nil)
		}

		var accountValue *bool
		for _, cap := range *apiRequest.SiteCapabilities {
			if len(cap.SiteIDs) == 0 {
				accountValue = cap.TargetedInstanceCreation
				break
			}
		}
		if accountValue == nil {
			return cutil.NewAPIError(http.StatusBadRequest, "siteCapabilities must include exactly one entry with empty siteIds", nil)
		}

		_, derr = taDAO.Update(ctx, tx, cdbm.TenantAccountUpdateInput{
			TenantAccountID: taID,
			Config: &cdbm.TenantAccountConfig{
				TargetedInstanceCreation: *accountValue,
			},
		})
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating TenantAccount config in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Tenant Account capabilities", nil)
		}

		tsDAO := cdbm.NewTenantSiteDAO(utah.dbSession)
		siteUpdates := map[uuid.UUID]bool{}
		siteIDs := map[uuid.UUID]struct{}{}

		for _, cap := range *apiRequest.SiteCapabilities {
			if len(cap.SiteIDs) == 0 {
				continue
			}
			for _, siteIDStr := range cap.SiteIDs {
				siteID, perr := uuid.Parse(siteIDStr)
				if perr != nil {
					return cutil.NewAPIError(http.StatusBadRequest, "Invalid Site ID in siteCapabilities", nil)
				}
				siteUpdates[siteID] = *cap.TargetedInstanceCreation
				siteIDs[siteID] = struct{}{}
			}
		}

		for siteID := range siteUpdates {
			ts, gerr := tsDAO.GetByTenantIDAndSiteID(ctx, tx, *ta.TenantID, siteID, []string{"Site"})
			if gerr != nil {
				if errors.Is(gerr, cdb.ErrDoesNotExist) {
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Site %s is not associated with Tenant", siteID.String()), nil)
				}
				logger.Error().Err(gerr).Msg("error retrieving TenantSite for siteCapabilities update")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Tenant Account capabilities", nil)
			}
			if ts.Site == nil || ts.Site.InfrastructureProviderID != ta.InfrastructureProviderID {
				return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Site %s is not owned by the Tenant Account Infrastructure Provider", siteID.String()), nil)
			}

			_, derr = tsDAO.Update(ctx, tx, cdbm.TenantSiteUpdateInput{
				TenantSiteID: ts.ID,
				Config: &cdbm.TenantSiteConfig{
					TargetedInstanceCreation: cutil.GetPtr(siteUpdates[siteID]),
				},
			})
			if derr != nil {
				logger.Error().Err(derr).Msg("error updating TenantSite config for siteCapabilities")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Tenant Account capabilities", nil)
			}
		}

		allTenantSites, _, gerr := tsDAO.GetAll(ctx, tx, cdbm.TenantSiteFilterInput{
			TenantIDs: []uuid.UUID{*ta.TenantID},
		}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, []string{"Site"})
		if gerr != nil {
			logger.Error().Err(gerr).Msg("error retrieving Tenant Sites for capability override cleanup")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Tenant Account capabilities", nil)
		}

		for _, ts := range allTenantSites {
			if ts.Site == nil || ts.Site.InfrastructureProviderID != ta.InfrastructureProviderID {
				continue
			}
			if _, listed := siteIDs[ts.SiteID]; listed {
				continue
			}
			if ts.Config.TargetedInstanceCreation == nil {
				continue
			}
			_, derr = tsDAO.Update(ctx, tx, cdbm.TenantSiteUpdateInput{
				TenantSiteID: ts.ID,
				Config:       &cdbm.TenantSiteConfig{},
			})
			if derr != nil {
				logger.Error().Err(derr).Msg("error clearing stale TenantSite capability override")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Tenant Account capabilities", nil)
			}
		}

		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to update Tenant Account capabilities")
	}

	ta, err = taDAO.GetByID(ctx, nil, taID, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving updated TenantAccount")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve updated Tenant Account", nil)
	}

	var tenantSites []cdbm.TenantSite
	if ta != nil && ta.TenantID != nil {
		tsDAO := cdbm.NewTenantSiteDAO(utah.dbSession)
		tenantSites, _, err = tsDAO.GetAll(ctx, nil, cdbm.TenantSiteFilterInput{
			TenantIDs: []uuid.UUID{*ta.TenantID},
		}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, []string{"Site"})
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Tenant Sites for updated Tenant Account")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant Sites for Tenant Account", nil)
		}
	}

	sdDAO := cdbm.NewStatusDetailDAO(utah.dbSession)
	ssds, _, err := sdDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{ta.ID.String()}}, cdbp.PageInput{Limit: cutil.GetPtr(pagination.MaxPageSize)})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Status Details for TenantAccount from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Status Details for TenantAccount", nil)
	}

	allocationCount := 0
	if ta.TenantID != nil {
		aDAO := cdbm.NewAllocationDAO(utah.dbSession)
		cnt, cerr := aDAO.GetCount(ctx, nil, cdbm.AllocationFilterInput{
			InfrastructureProviderIDs: []uuid.UUID{ta.InfrastructureProviderID},
			TenantIDs:                 []uuid.UUID{*ta.TenantID},
		})
		if cerr != nil {
			logger.Error().Err(cerr).Msg("error retrieving allocation count for updated Tenant Account")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve allocation count for Tenant Account", nil)
		}
		allocationCount = cnt
	}

	apiInstance := model.NewAPITenantAccount(ta, ssds, allocationCount, tenantSites)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiInstance)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteTenantAccountHandler is the API Handler for deleting a TenantAccount
type DeleteTenantAccountHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewDeleteTenantAccountHandler initializes and returns a new handler for deleting Tenant
func NewDeleteTenantAccountHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) DeleteTenantAccountHandler {
	return DeleteTenantAccountHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Delete an existing TenantAccount
// @Description Delete an existing TenantAccount
// @Tags tenantaccount
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Tenant Account"
// @Success 202
// @Router /v2/org/{org}/nico/tenant/account/{id} [delete]
func (dtah DeleteTenantAccountHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TenantAccount", "Delete", c)
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

	// Validate role, only Provider Admins are allowed to delete TenantAccounts
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get tenant account ID from URL param
	taStrID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("tenantaccount_id", taStrID))

	taID, err := uuid.Parse(taStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Tenant Account ID in URL", nil)
	}

	logger.Info().Str("tenantaccount", taStrID).Msg("deleting tenant account")

	// Check that TenantAccount exists
	taDAO := cdbm.NewTenantAccountDAO(dtah.dbSession)

	ta, err := taDAO.GetByID(ctx, nil, taID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find TenantAccount with specified ID", nil)
		}
		logger.Error().Str("tenantaccount", taID.String()).Err(err).Msg("error retrieving TenantAccount DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Could not retrieve TenantAccount", nil)
	}

	// Check that the org's infrastructureProvider matches infrastructureProvider in TenantAccount
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, dtah.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error getting InfrastructureProvider for Org", nil)
	}
	if ip.ID != ta.InfrastructureProviderID {
		logger.Warn().Msg("infrastructureProvider in org does not match infrastructureProvider in tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "InfrastructureProvider for Org does not match InfrastructureProvider in TenantAccount", nil)
	}

	if ta.TenantID != nil {
		// Verify that Tenant does not have any Allocations with the Provider
		allocationDAO := cdbm.NewAllocationDAO(dtah.dbSession)
		allocationFilter := cdbm.AllocationFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}}
		if ta.TenantID != nil {
			allocationFilter.TenantIDs = append(allocationFilter.TenantIDs, *ta.TenantID)
		}
		aCount, err := allocationDAO.GetCount(ctx, nil, allocationFilter)
		if err != nil {
			logger.Error().Str("ip", ip.ID.String()).Str("tenant", ta.TenantID.String()).Err(err).Msg("error getting allocations")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Error getting allocations for InfrastructureProvider, Tenant", nil)
		}

		if aCount > 0 {
			logger.Warn().Str("tenant", ta.TenantID.String()).Msg("allocations exist for tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Allocations exist for Tenant", nil)
		}
	}

	// Delete TenantAccount in DB
	err = taDAO.Delete(ctx, nil, taID)
	if err != nil {
		logger.Error().Err(err).Msg("error deleting TenantAccount in DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to delete Tenant", nil)
	}

	// Create response
	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusAccepted, model.NewAPIDeletionAcceptedResponse())
}
