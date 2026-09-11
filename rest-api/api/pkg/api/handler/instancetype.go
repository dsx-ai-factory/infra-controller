// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"slices"
	"strconv"

	"go.opentelemetry.io/otel/attribute"
	temporalClient "go.temporal.io/sdk/client"
	tp "go.temporal.io/sdk/temporal"

	mapset "github.com/deckarep/golang-set/v2"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"

	"github.com/labstack/echo/v4"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"

	cwma "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/machine"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
)

// ~~~~~ Create Handler ~~~~~ //

// CreateInstanceTypeHandler is the API Handler for creating new InstanceType
type CreateInstanceTypeHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewCreateInstanceTypeHandler initializes and returns a new handler for creating Instance Type
func NewCreateInstanceTypeHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) CreateInstanceTypeHandler {
	return CreateInstanceTypeHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Create an Instance Type for a Site
// @Description Create an Instance Type for a Site. Only Infrastructure Providers can create an Instance Type
// @Tags instancetype
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIInstanceTypeCreateRequest true "Instance Type create request"
// @Success 201 {object} model.APIInstanceType
// @Router /v2/org/{org}/nico/instance/type [post]
func (cith CreateInstanceTypeHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("InstanceType", "Create", c)
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

	// Validate role, only Provider Admins are allowed to create Instance Types
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Validate request data
	// Bind request data to API model
	apiRequest := model.APIInstanceTypeCreateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Instance Type creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Instance Type creation request data", verr)
	}

	// Get Infrastructure Provider for the Org
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, cith.dbSession, org)
	if err != nil {
		logger.Error().Err(err).Msg("error getting Infrastructure Provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to get Infrastructure Provider for org", nil)
	}

	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, cith.dbSession)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Site specified in request")
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find Site specified in request", nil)
		}
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request", nil)
	}

	if site.InfrastructureProviderID != ip.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request is not associated with the Infrastructure Provider for the Org", nil)
	}

	// Check if an Instance Type already exists for the given name and Site ID
	itDAO := cdbm.NewInstanceTypeDAO(cith.dbSession)
	ists, tot, err := itDAO.GetAll(ctx, nil, cdbm.InstanceTypeFilterInput{Name: &apiRequest.Name, InfrastructureProviderID: &ip.ID, SiteIDs: []uuid.UUID{site.ID}}, nil, nil, nil, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error checking for existing Instance Types")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to check for existing Instance Types", nil)
	}
	if tot > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, fmt.Sprintf("Instance Type with name: %s for Site: %s already exists", apiRequest.Name, apiRequest.SiteID), validation.Errors{
			"id": errors.New(ists[0].ID.String()),
		})
	}

	// Labels support
	// Initialize labels map to empty map ({}) if no labels are provided
	labels := make(map[string]string)
	if apiRequest.Labels != nil {
		labels = apiRequest.Labels
	}

	mcDAO := cdbm.NewMachineCapabilityDAO(cith.dbSession)
	sdDAO := cdbm.NewStatusDetailDAO(cith.dbSession)

	var (
		it          *cdbm.InstanceType
		ssd         *cdbm.StatusDetail
		mcs         []cdbm.MachineCapability
		timeoutResp func() error
	)

	err = cdb.WithTx(ctx, cith.dbSession, func(tx *cdb.Tx) error {
		// Create Instance Type
		var derr error
		it, derr = itDAO.Create(ctx, tx, cdbm.InstanceTypeCreateInput{
			Name:                     apiRequest.Name,
			Description:              apiRequest.Description,
			ControllerMachineType:    apiRequest.ControllerMachineType,
			InfrastructureProviderID: ip.ID,
			SiteID:                   &site.ID,
			Labels:                   labels,
			Status:                   cdbm.InstanceTypeStatusReady,
			CreatedBy:                dbUser.ID,
		})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Instance Type")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Instance Type", nil)
		}

		// Create Machine Capabilities if it is provided
		if apiRequest.MachineCapabilities != nil {
			for i, apimc := range apiRequest.MachineCapabilities {
				mcinfo := map[string]interface{}{}
				if apimc.Cores != nil {
					mcinfo[cwma.MachineCPUCoreCount] = *apimc.Cores
				}
				if apimc.Threads != nil {
					mcinfo[cwma.MachineCPUThreadCount] = *apimc.Threads
				}
				_, serr := mcDAO.Create(ctx, tx, cdbm.MachineCapabilityCreateInput{
					Index:            i,
					InstanceTypeID:   &it.ID,
					Type:             apimc.Type,
					Name:             apimc.Name,
					Frequency:        apimc.Frequency,
					Capacity:         apimc.Capacity,
					Vendor:           apimc.Vendor,
					HardwareRevision: apimc.HardwareRevision,
					Count:            apimc.Count,
					DeviceType:       apimc.DeviceType,
					InactiveDevices:  apimc.InactiveDevices,
					Info:             mcinfo,
				})
				if serr != nil {
					logger.Error().Err(serr).Msg("error creating Machine Capability")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Machine Capability for Instance Type", nil)
				}
			}
		}

		// Create a status detail record for Instance Type
		var serr error
		ssd, serr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: it.ID.String(), Status: *cutil.GetPtr(cdbm.InstanceTypeStatusReady), Message: cutil.GetPtr("Instance type is ready for use")})
		if serr != nil {
			logger.Error().Err(serr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for Instance Type", nil)
		}
		if ssd == nil {
			logger.Error().Msg("Status Detail DB entry not returned from Create")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to get new Status Detail for Instance Type", nil)
		}

		// Get Machine capabilities for the Instance Type
		mcs, _, derr = mcDAO.GetAll(ctx, tx, nil, []uuid.UUID{it.ID}, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Machine capabilities for Instance Type")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Machine capabilities for Instance Type", nil)
		}

		// Get the temporal client for the site we are working with.
		stc, derr := cith.scp.GetClientByID(site.ID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		// Attach the loaded capabilities to the entity so
		// `apiRequest.ToProto(it)` can serialise them. The entity owns
		// the Index-sort that NICo's update semantics require; per-cap
		// wire mapping lives on `(*MachineCapability).ToProto` and the
		// type / device-type / numeric-bounds rules were enforced by
		// `apiRequest.Validate`.
		it.AttachCapabilities(mcs)

		createInstanceTypeRequest := apiRequest.ToProto(it)

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "instance-type-create-" + it.ID.String(),
			TaskQueue:                queue.SiteTaskQueue,
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		}

		logger.Info().Msg("triggering InstanceType create workflow")

		// Add context deadlines
		wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow
		we, wferr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "CreateInstanceType", createInstanceTypeRequest)
		if wferr != nil {
			logger.Error().Err(wferr).Msg("failed to synchronously start Temporal workflow to create InstanceType")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed start sync workflow to create InstanceType on Site: %s", wferr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous create InstanceType workflow")

		// Block until the workflow has completed and returned success/error.
		wferr = we.Get(wfCtx, nil)

		// Handle skippable errors
		if wferr != nil {
			var applicationErr *tp.ApplicationError
			// NICo _could_ respond with an unimplemented if it's an
			// older NICo that doesn't have the endpoint yet, but it's more
			// likely to respond with a PermissionDenied because the permission
			// for the path isn't there either, but we can watch for both just to
			// be safe.
			if errors.As(wferr, &applicationErr) && (slices.Contains(swe.UnimplementedOrDeniedErrTypes(), applicationErr.Type())) {
				logger.Warn().Msg("NICo endpoint unimplemented or restricted response received from Site")
				// Reset error to nil because we'll want to ignore while
				// NICo is being rolled out.
				wferr = nil
			}
		}

		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || wfCtx.Err() != nil {
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "InstanceType", "CreateInstanceType")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "workflow timeout", nil)
			}

			code, uwerr := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(uwerr).Msg("failed to synchronously execute Temporal workflow to create InstanceType")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to create InstanceType on Site: %s", uwerr), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous create InstanceType workflow")
		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to create Instance Type, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// Create API response
	ait := model.NewAPIInstanceType(it, []cdbm.StatusDetail{*ssd}, mcs, nil, nil)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusCreated, ait)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllInstanceTypeHandler is the API Handler for getting all Instance Types
type GetAllInstanceTypeHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetAllInstanceTypeHandler initializes and returns a new handler for getting all Instance Types
func NewGetAllInstanceTypeHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllInstanceTypeHandler {
	return GetAllInstanceTypeHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get all Instance Types
// @Description Get all Instance Types relevant to current org
// @Tags instancetype
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string false "ID of Site"
// @Param infrastructureProviderId query string false "Deprecated: ID of Infrastructure Provider"
// @Param tenantId query string false "Deprecated: ID of Tenant"
// @Param status query string false "Query input for status"
// @Param query query string false "Query input for full text search"
// @Param includeAllocationStats query boolean false "Allocation stats to include in response"
// @Param includeMachineAssignment query boolean false "Machine associations entity to include in response (Provider only)"
// @Param excludeUnallocated query boolean false "Exclude unallocated Instance Types (Tenant only)"
// @Param includeRelation query string false "Related entities to include in response e.g. 'InfrastructureProvider', 'Tenant', 'Site'"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APIInstanceType
// @Router /v2/org/{org}/nico/instance/type [get]
func (gaith GetAllInstanceTypeHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("InstanceType", "GetAll", c)
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
	err = pageRequest.Validate(cdbm.InstanceTypeOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Get Site ID from query
	var st *cdbm.Site
	var stID *uuid.UUID
	qstID := c.QueryParam("siteId")
	if qstID != "" {
		siteID, err := uuid.Parse(qstID)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Site ID in query", nil)
		}
		stID = &siteID

		// Get Site from DB if Site ID is provided
		stDAO := cdbm.NewSiteDAO(gaith.dbSession)
		st, err = stDAO.GetByID(ctx, nil, *stID, nil, false)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Site from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in query", nil)
		}
	}

	// Check `includeMachineAssignment` in query
	includeMachineAssignment := false
	qima := c.QueryParam("includeMachineAssignment")
	if qima != "" {
		includeMachineAssignment, err = strconv.ParseBool(qima)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `includeMachineAssignment` query param", nil)
		}
	}

	// Check `includeAllocationStats` in query
	includeAllocationStats := false
	qias := c.QueryParam("includeAllocationStats")
	if qias != "" {
		includeAllocationStats, err = strconv.ParseBool(qias)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `includeAllocationStats` query param", nil)
		}
	}

	// Check `excludeUnallocated` in query
	excludeUnallocated := false
	qias = c.QueryParam("excludeUnallocated")
	if qias != "" {
		excludeUnallocated, err = strconv.ParseBool(qias)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `excludeUnallocated` query param", nil)
		}
	}

	// Get query text for full text search from query param
	searchQuery := common.GetSearchQuery(c)
	if searchQuery != nil {
		cotel.SetAttribute(handlerSpan, attribute.String("query", *searchQuery))
	}

	// Get status from query param
	var status *string

	statusQuery := c.QueryParam("status")
	if statusQuery != "" {
		_, ok := cdbm.InstanceTypeStatusMap[statusQuery]
		if !ok {
			logger.Warn().Msg(fmt.Sprintf("invalid value in status query: %v", statusQuery))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Status value in query", nil)
		}
		status = &statusQuery
		cotel.SetAttribute(handlerSpan, attribute.String("status", statusQuery))
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.InstanceTypeRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	provider, tenant, apiErr := common.IsProviderOrTenant(ctx, logger, gaith.dbSession, org, dbUser, true, nil)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	// Perspective-specific query param restrictions
	if includeMachineAssignment && provider == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Only Provider can specify query param `includeMachineAssignment`", nil)
	}
	if excludeUnallocated && tenant == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Only Tenant can specify query param `excludeUnallocated`", nil)
	}

	// Collect instance type IDs from both Provider and Tenant perspectives
	itDAO := cdbm.NewInstanceTypeDAO(gaith.dbSession)
	tsDAO := cdbm.NewTenantSiteDAO(gaith.dbSession)
	mergedInstanceTypeIDs := mapset.NewSet[uuid.UUID]()

	var tenantID *uuid.UUID

	if provider != nil {
		providerSiteMatch := st == nil || st.InfrastructureProviderID == provider.ID
		if !providerSiteMatch {
			logger.Info().Msg("skipping provider perspective: site not owned by provider")
		} else {
			var providerSiteIDs []uuid.UUID
			if st != nil {
				providerSiteIDs = []uuid.UUID{*stID}
			}

			providerFilter := cdbm.InstanceTypeFilterInput{
				InfrastructureProviderID: &provider.ID,
				SiteIDs:                  providerSiteIDs,
				Status:                   status,
				SearchQuery:              searchQuery,
			}
			providerInstanceTypes, _, err := itDAO.GetAll(ctx, nil, providerFilter, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
			if err != nil {
				logger.Error().Err(err).Msg("error retrieving Instance Types from Provider perspective")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instance Types, DB error", nil)
			}
			for _, it := range providerInstanceTypes {
				mergedInstanceTypeIDs.Add(it.ID)
			}
		}
	}

	if tenant != nil {
		tenantID = &tenant.ID

		var tenantSiteIDs []uuid.UUID
		skipTenantQuery := false
		if stID == nil {
			tss, _, err := tsDAO.GetAll(ctx, nil, cdbm.TenantSiteFilterInput{
				TenantIDs: []uuid.UUID{tenant.ID},
			}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
			if err != nil {
				logger.Error().Err(err).Msg("error retrieving Tenant Site association from DB")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant Site association", nil)
			}
			for _, ts := range tss {
				tenantSiteIDs = append(tenantSiteIDs, ts.SiteID)
			}
			if len(tenantSiteIDs) == 0 {
				logger.Info().Msg("skipping tenant perspective: tenant has no site associations")
				skipTenantQuery = true
			}
		} else {
			_, err = tsDAO.GetByTenantIDAndSiteID(ctx, nil, tenant.ID, *stID, nil)
			if err != nil {
				if err == cdb.ErrDoesNotExist {
					logger.Info().Msg("skipping tenant perspective: tenant not associated with site")
					skipTenantQuery = true
				} else {
					logger.Error().Err(err).Msg("error retrieving Tenant Site association from DB")
					return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to determine if Tenant is associated with Site, DB error", nil)
				}
			} else {
				tenantSiteIDs = []uuid.UUID{*stID}
			}
		}

		if !skipTenantQuery {
			tenantFilter := cdbm.InstanceTypeFilterInput{
				SiteIDs:     tenantSiteIDs,
				Status:      status,
				SearchQuery: searchQuery,
			}
			if excludeUnallocated {
				tenantFilter.TenantIDs = []uuid.UUID{tenant.ID}
			}
			tenantInstanceTypes, _, err := itDAO.GetAll(ctx, nil, tenantFilter, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
			if err != nil {
				logger.Error().Err(err).Msg("error retrieving Instance Types from Tenant perspective")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instance Types, DB error", nil)
			}
			for _, it := range tenantInstanceTypes {
				mergedInstanceTypeIDs.Add(it.ID)
			}
		}
	}

	// Final paginated query using the merged instance type IDs
	its, total, err := itDAO.GetAll(ctx, nil, cdbm.InstanceTypeFilterInput{
		InstanceTypeIDs: mergedInstanceTypeIDs.ToSlice(),
	}, qIncludeRelations, pageRequest.Offset, pageRequest.Limit, pageRequest.OrderBy)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Instance Types for Site specified in query")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instance Types for Site in query", nil)
	}

	// Get status details
	sdDAO := cdbm.NewStatusDetailDAO(gaith.dbSession)

	sdEntityIDs := []string{}
	for _, it := range its {
		sdEntityIDs = append(sdEntityIDs, it.ID.String())
	}
	ssds, serr := sdDAO.GetRecentByEntityIDs(ctx, nil, sdEntityIDs, common.RECENT_STATUS_DETAIL_COUNT)
	if serr != nil {
		logger.Warn().Err(serr).Msg("error retrieving Status Details for Instance Types from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate status history for Instance Types", nil)
	}
	ssdMap := map[string][]cdbm.StatusDetail{}
	for _, ssd := range ssds {
		cssd := ssd
		ssdMap[ssd.EntityID] = append(ssdMap[ssd.EntityID], cssd)
	}

	// Create response
	aits := make([]model.APIInstanceType, 0, len(its))

	mcDAO := cdbm.NewMachineCapabilityDAO(gaith.dbSession)
	mitDAO := cdbm.NewMachineInstanceTypeDAO(gaith.dbSession)

	itIDs := []uuid.UUID{}

	for _, it := range its {
		itIDs = append(itIDs, it.ID)
	}

	mcs, _, serr := mcDAO.GetAll(ctx, nil, nil, itIDs, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
	if serr != nil {
		logger.Error().Err(serr).Msg("error retrieving Machine Capabilities for Instance Type from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Machine Capabilities for Instance Type", nil)
	}

	itMcsMap := map[uuid.UUID][]cdbm.MachineCapability{}
	for _, mc := range mcs {
		itMcsMap[*mc.InstanceTypeID] = append(itMcsMap[*mc.InstanceTypeID], mc)
	}
	var mit []cdbm.MachineInstanceType
	if includeMachineAssignment {
		mitIDs := make([]uuid.UUID, 0, len(its))
		for i := range its {
			if its[i].InfrastructureProviderID == provider.ID {
				mitIDs = append(mitIDs, its[i].ID)
			}
		}
		if len(mitIDs) > 0 {
			mit, _, err = mitDAO.GetAll(ctx, nil, cdbm.MachineInstanceTypeFilterInput{InstanceTypeIDs: mitIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
			if err != nil {
				logger.Error().Err(err).Msg("error retrieving Machine assignments for Instance Type")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve  Machine assignments for Instance Type", nil)
			}
		}
	}
	instanceTypeIDsToMachineInstanceTypeMap := map[uuid.UUID][]cdbm.MachineInstanceType{}

	for _, mi := range mit {
		cmi := mi
		instanceTypeIDsToMachineInstanceTypeMap[mi.InstanceTypeID] = append(instanceTypeIDsToMachineInstanceTypeMap[mi.InstanceTypeID], cmi)
	}

	var instantTypeIDsToAllocStatsMap map[uuid.UUID]*model.APIInstanceTypeAllocationStats
	if includeAllocationStats {
		instantTypeIDsToAllocStatsMap, apiErr = common.GetAllInstanceTypeAllocationStats(ctx, gaith.dbSession, stID, itIDs, logger, tenantID)
		if apiErr != nil {
			return c.JSON(apiErr.Code, apiErr)
		}
	}

	for _, it := range its {
		cit := it
		allocStat := instantTypeIDsToAllocStatsMap[it.ID]

		machineInstanceTypes := instanceTypeIDsToMachineInstanceTypeMap[it.ID]

		ait := model.NewAPIInstanceType(&cit, ssdMap[it.ID.String()], itMcsMap[it.ID], machineInstanceTypes, allocStat)
		aits = append(aits, *ait)
	}

	// Create pagination response header
	pageReponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageReponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}

	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	// Create response
	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, aits)
}

// ~~~~~ Get Handler ~~~~~ //

// GetInstanceTypeHandler is the API Handler for getting details of a specific instance type
type GetInstanceTypeHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetInstanceTypeHandler initializes and returns a new handler for getting an Instance Type
func NewGetInstanceTypeHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetInstanceTypeHandler {
	return GetInstanceTypeHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get details of an Instance Type
// @Description Retrieve details of a specific Instance Type by ID
// @Tags instancetype
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id query string true "ID of Instance Type"
// @Param includeAllocationStats query boolean false "Allocation stats to include in response"
// @Param includeMachineAssignment query boolean false "Machine associations entity to include in response (Provider only)"
// @Param includeRelation query string false "Related entities to include in response e.g. 'InfrastructureProvider', 'Site'"
// @Success 200 {object} []model.APIInstanceType
// @Router /v2/org/{org}/nico/instance/type/{id} [get]
func (gith GetInstanceTypeHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("InstanceType", "Get", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Get Instance Type ID
	itStrID := c.Param("id")

	itID, err := uuid.Parse(itStrID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Instance Type ID in URL", nil)
	}

	cotel.SetAttribute(handlerSpan, attribute.String("instancetype_id", itStrID))

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.InstanceTypeRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Check `includeMachineAssignment` in query
	includeMachineAssignment := false
	qimag := c.QueryParam("includeMachineAssignment")
	if qimag != "" {
		includeMachineAssignment, err = strconv.ParseBool(qimag)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `includeMachineAssignment` query param", nil)
		}
	}

	// Check `includeAllocationStats` in query
	includeAllocationStats := false
	qiasg := c.QueryParam("includeAllocationStats")
	if qiasg != "" {
		includeAllocationStats, err = strconv.ParseBool(qiasg)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `includeAllocationStats` query param", nil)
		}
	}

	provider, tenant, apiErr := common.IsProviderOrTenant(ctx, logger, gith.dbSession, org, dbUser, true, nil)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	if includeMachineAssignment && provider == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Only Provider can specify query param `includeMachineAssignment`", nil)
	}

	// Get Instance Type
	itDAO := cdbm.NewInstanceTypeDAO(gith.dbSession)

	it, err := itDAO.GetByID(ctx, nil, itID, qIncludeRelations)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find InstanceType with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Instance Type from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instance Type", nil)
	}

	// Check if Instance Type is associated with Provider or Tenant.
	// Provider check is a direct ID comparison; tenant check requires a TenantSite DB lookup
	// since tenants access instance types via site association, not direct ownership.
	tsDAO := cdbm.NewTenantSiteDAO(gith.dbSession)
	var tenantID *uuid.UUID

	authorizedViaProvider := provider != nil && it.InfrastructureProviderID == provider.ID

	if authorizedViaProvider {
		// authorized via provider ownership — fall through
	} else if tenant != nil && it.SiteID != nil {
		_, err = tsDAO.GetByTenantIDAndSiteID(ctx, nil, tenant.ID, *it.SiteID, nil)
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Instance Type is not associated with org", nil)
		} else if err != nil {
			logger.Error().Err(err).Msg("error retrieving Tenant Site association from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to determine if Tenant has access to Instance Type, DB error", nil)
		}
		tenantID = &tenant.ID
	} else {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Instance Type is not associated with org", nil)
	}

	if includeMachineAssignment && !authorizedViaProvider {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Only Provider can specify query param `includeMachineAssignment`", nil)
	}

	// Get Instance Type status details
	sdDAO := cdbm.NewStatusDetailDAO(gith.dbSession)

	ssds, serr := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{it.ID.String()}, common.RECENT_STATUS_DETAIL_COUNT)
	if serr != nil {
		logger.Error().Err(serr).Msg("error retrieving Status Details for Instance Type from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve status history for Instance Type", nil)
	}

	// Get Machine capabilities for the Instance Type
	mcDAO := cdbm.NewMachineCapabilityDAO(gith.dbSession)
	mcs, _, err := mcDAO.GetAll(ctx, nil, nil, []uuid.UUID{it.ID}, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(pagination.MaxPageSize), nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Machine capabilities for Instance Type")
		// rollback transaction
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Machine capabilities for Instance Type", nil)
	}

	// Get Machine Instance Type for this Instance Type.
	var mit []cdbm.MachineInstanceType
	if includeMachineAssignment {
		// Check if Machine/InstanceType association already exists
		mitDAO := cdbm.NewMachineInstanceTypeDAO(gith.dbSession)
		mit, _, err = mitDAO.GetAll(ctx, nil, cdbm.MachineInstanceTypeFilterInput{InstanceTypeIDs: []uuid.UUID{it.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Machine assignments for Instance Type")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve  Machine assignments for Instance Type", nil)
		}
	}

	// Allocation stats info for this Tnstance Type
	var aas *model.APIInstanceTypeAllocationStats

	if includeAllocationStats {
		aas, apiErr = common.GetInstanceTypeAllocationStats(ctx, gith.dbSession, logger, *it, tenantID)
		if apiErr != nil {
			return c.JSON(apiErr.Code, apiErr)
		}
	}

	ait := model.NewAPIInstanceType(it, ssds, mcs, mit, aas)

	// Create response
	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, ait)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateInstanceTypeHandler is the API Handler for updating an Instance Type
type UpdateInstanceTypeHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewUpdateInstanceTypeHandler initializes and returns a new handler for updating Instance Type
func NewUpdateInstanceTypeHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) UpdateInstanceTypeHandler {
	return UpdateInstanceTypeHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Update an existing Instance Type
// @Description Update an existing Instance Type. Org's Infrastructure Provider must be associated with the Instance Type.
// @Tags instancetype
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id query string true "ID of Instance Type"
// @Param message body model.APIInstanceTypeUpdateRequest true "Instance Type update request"
// @Success 200 {object} model.APIInstanceType
// @Router /v2/org/{org}/nico/instance/type/{id} [patch]
func (uith UpdateInstanceTypeHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("InstanceType", "Update", c)
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

	// Validate role, only Provider Admins are allowed to proceed from here
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Instance Type ID
	itStrID := c.Param("id")

	itID, err := uuid.Parse(itStrID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Instance Type ID in URL", nil)
	}

	cotel.SetAttribute(handlerSpan, attribute.String("instancetype_id", itStrID))

	// Check if org has an Infrastructure Provider
	ipDAO := cdbm.NewInfrastructureProviderDAO(uith.dbSession)

	ips, err := ipDAO.GetAllByOrg(ctx, nil, org, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Infrastructure Provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to to retrieve Infrastructure Provider for org", nil)
	}

	if len(ips) == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have an Infrastructure Provider", nil)
	}

	orgIP := &ips[0]

	// Get Instance Type
	itDAO := cdbm.NewInstanceTypeDAO(uith.dbSession)

	it, err := itDAO.GetByID(ctx, nil, itID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Instance Type with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Instance Type from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instance Type", nil)
	}

	// Check if Instance Type is associated with the Org's Provider
	if orgIP.ID != it.InfrastructureProviderID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Instance Type is not associated with org's Infrastructure Provider", nil)
	}

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIInstanceTypeUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Instance Type update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Instance Type update request data", verr)
	}

	// Check for name uniqueness, i.e. there cannot be another Instance Type with same name for the same Site
	if apiRequest.Name != nil && *apiRequest.Name != it.Name {
		ists, tot, serr := itDAO.GetAll(ctx, nil, cdbm.InstanceTypeFilterInput{Name: apiRequest.Name, InfrastructureProviderID: &it.InfrastructureProviderID, SiteIDs: []uuid.UUID{*it.SiteID}}, nil, nil, nil, nil)
		if serr != nil {
			logger.Error().Err(serr).Msg("error checking for existing Instance Types")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to check for existing Instance Types", nil)
		}
		if tot > 0 && ists[0].ID != it.ID {
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Another Instance Type with specified name already exists for Site and Provider", validation.Errors{
				"id": errors.New(ists[0].ID.String()),
			})
		}
	}

	mcDAO := cdbm.NewMachineCapabilityDAO(uith.dbSession)
	sdDAO := cdbm.NewStatusDetailDAO(uith.dbSession)

	var (
		mcs         []cdbm.MachineCapability
		ssds        []cdbm.StatusDetail
		timeoutResp func() error
	)

	err = cdb.WithTx(ctx, uith.dbSession, func(tx *cdb.Tx) error {
		// Acquire an advisory lock on this Instance Type so the machine-association
		// check + capability recreation below is serialized per Instance Type.
		// Other handlers that mutate this Instance Type (Delete, future capability
		// updates) take the same lock; the Machine attach path in machine.go also
		// needs a complementary lock on the new Instance Type to fully prevent
		// concurrent attaches racing with capability updates.
		derr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(it.ID.String()), nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to acquire advisory lock on Instance Type")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Instance Type", nil)
		}

		if apiRequest.MachineCapabilities != nil {

			// If a capabilities update was requested, we'll need to make sure
			// we don't have associated machines.

			// Get the machines for instance type
			mDAO := cdbm.NewMachineDAO(uith.dbSession)

			// We hold an advisory lock on this Instance Type (acquired above), so a
			// concurrent capability-update or delete on the same Instance Type can't
			// race with us. The Machine attach path in machine.go does not currently
			// take a lock on the new Instance Type it's attaching to, so an attach
			// can still slip in between this check and our updates — see the lock
			// comment above for the complementary fix.
			_, total, derr := mDAO.GetAll(ctx, tx, cdbm.MachineFilterInput{InstanceTypeIDs: []uuid.UUID{it.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(0)}, nil)
			if derr != nil {
				logger.Error().Err(derr).Msg("error retrieving Machines for Instance Type from DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Machines for Instance Type", nil)
			}

			if total > 0 {
				logger.Error().Msg("MachineCapabilities cannot be updated when there are associated Machines")
				return cutil.NewAPIError(http.StatusPreconditionFailed, "MachineCapabilities cannot be updated when there are associated Machines", nil)
			}

			// If we got here, then we're allowed to update the capabilities.

			// Get Machine capabilities for the Instance Type because we'll need to compare.
			existingMcs, _, derr := mcDAO.GetAll(ctx, tx, nil, []uuid.UUID{it.ID}, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(pagination.MaxPageSize), nil)
			if derr != nil {
				logger.Error().Err(derr).Msg("error retrieving Machine capabilities for Instance Type")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Machine capabilities for Instance Type", nil)
			}

			addCapabilities := []*cdbm.MachineCapabilityCreateInput{}

			// Create a map to track capabilities that already exist
			// in the DB so we can compare against the incoming request.
			existingMacCapMap := map[string]*cdbm.MachineCapability{}
			for i := range existingMcs {
				mc := &existingMcs[i]
				existingMacCapMap[mc.MapKey()] = mc
			}

			for pos, reqMacCap := range apiRequest.MachineCapabilities {
				capKey := reqMacCap.MapKey()

				existingCap := existingMacCapMap[capKey]
				// The incoming requested capability doesn't exist at all currently,
				// so it's brand new.
				if existingCap != nil && existingCap.Equal(&cdbm.MachineCapability{
					Type:             reqMacCap.Type,
					Name:             reqMacCap.Name,
					Frequency:        reqMacCap.Frequency,
					Capacity:         reqMacCap.Capacity,
					HardwareRevision: reqMacCap.HardwareRevision,
					Cores:            reqMacCap.Cores,
					Threads:          reqMacCap.Threads,
					Vendor:           reqMacCap.Vendor,
					Count:            reqMacCap.Count,
					DeviceType:       reqMacCap.DeviceType,
					InactiveDevices:  reqMacCap.InactiveDevices,
					Index:            pos,
				}) {
					// If the capability existed and the requested settings
					// and existing settings matched, it shouldn't be touched,
					// so delete from the map so we're left with only entries
					// that need to be deleted.
					delete(existingMacCapMap, capKey)

				} else {
					// If there's no existing capability for the one in the request or the
					// capabilities were not equal, then we'll recreate with the new values.
					// Build Info map (consistent with Create handler) so downstream
					// workflow activities see the same shape regardless of whether
					// the capability was created via Create or recreated via Update.
					mcinfo := map[string]interface{}{}
					if reqMacCap.Cores != nil {
						mcinfo[cwma.MachineCPUCoreCount] = *reqMacCap.Cores
					}
					if reqMacCap.Threads != nil {
						mcinfo[cwma.MachineCPUThreadCount] = *reqMacCap.Threads
					}
					addCapabilities = append(addCapabilities, &cdbm.MachineCapabilityCreateInput{
						InstanceTypeID:   &it.ID,
						Type:             reqMacCap.Type,
						Name:             reqMacCap.Name,
						Frequency:        reqMacCap.Frequency,
						Capacity:         reqMacCap.Capacity,
						HardwareRevision: reqMacCap.HardwareRevision,
						Cores:            reqMacCap.Cores,
						Threads:          reqMacCap.Threads,
						Vendor:           reqMacCap.Vendor,
						Count:            reqMacCap.Count,
						DeviceType:       reqMacCap.DeviceType,
						InactiveDevices:  reqMacCap.InactiveDevices,
						Info:             mcinfo,
						Index:            pos,
					})
				}
			}

			// Now we can remove the capabilities that changed or were deleted
			for _, macCap := range existingMacCapMap {
				serr := mcDAO.DeleteByID(ctx, tx, macCap.ID, false)
				if serr != nil {
					logger.Error().Err(serr).Msg("error deleting Machine Capability")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Machine Capability for Instance Type", nil)
				}
			}

			// Now we can add the capabilities that are new or updated
			for _, mc := range addCapabilities {
				_, serr := mcDAO.Create(ctx, tx, *mc)
				if serr != nil {
					logger.Error().Err(serr).Msg("error creating Machine Capability")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Machine Capability for Instance Type", nil)
				}
			}
		}

		// Update Instance Type
		it, derr = itDAO.Update(ctx, tx, cdbm.InstanceTypeUpdateInput{ID: itID, Name: apiRequest.Name, Description: apiRequest.Description, Labels: apiRequest.Labels})
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating Instance Type in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Instance Type", nil)
		}

		// Get the most up-to-date capabilities for the instance type
		mcs, _, derr = mcDAO.GetAll(ctx, tx, nil, []uuid.UUID{it.ID}, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Machine capabilities for Instance Type")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Machine capabilities for Instance Type", nil)
		}

		// Return API response

		// Get Instance Type status details
		var serr error
		ssds, _, serr = sdDAO.GetAll(ctx, tx, cdbm.StatusDetailFilterInput{EntityIDs: []string{it.ID.String()}}, cdbp.PageInput{Limit: cutil.GetPtr(pagination.MaxPageSize)})
		if serr != nil {
			logger.Error().Err(serr).Msg("error retrieving Status Details for Instance Type from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve status history for Instance Type", nil)
		}

		// Get the temporal client for the site we are working with.
		stc, derr := uith.scp.GetClientByID(*it.SiteID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		// Sort the capabilities list and attach to the entity. NICo will
		// deny updates if an InstanceType is associated with machines and
		// a change in capabilities is attempted, and order matters. Once
		// sorted, the slice is hung off `it.Capabilities` so
		// `apiRequest.ToProto(it)` can read it via the entity's
		// canonical `ToProto`; per-capability wire mapping lives on
		// `(*MachineCapability).ToProto` and the type / device-type /
		// numeric-bounds rules were enforced by `apiRequest.Validate`.
		slices.SortFunc(mcs, func(a, b cdbm.MachineCapability) int {
			return a.Index - b.Index
		})
		it.Capabilities = make([]*cdbm.MachineCapability, len(mcs))
		for i := range mcs {
			it.Capabilities[i] = &mcs[i]
		}

		updateInstanceTypeRequest := apiRequest.ToProto(it)

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "instance-type-update-" + it.ID.String(),
			TaskQueue:                queue.SiteTaskQueue,
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		}

		logger.Info().Msg("triggering InstanceType update workflow")

		// Add context deadlines
		wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow
		we, wferr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "UpdateInstanceType", updateInstanceTypeRequest)
		if wferr != nil {
			logger.Error().Err(wferr).Msg("failed to synchronously start Temporal workflow to update InstanceType")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed start sync workflow to update InstanceType on Site: %s", wferr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous update InstanceType workflow")

		// Block until the workflow has completed and returned success/error.
		wferr = we.Get(wfCtx, nil)

		// Handle skippable errors
		if wferr != nil {
			// TODO:
			// If this was a 404 back from NICo, we'll need to ignore until the
			// process for syncing cloud to site is done and the SOT has fully moved
			// to NICo.  At that point, all of these guard rails should be removed
			// and it should no longer be possible for the cloud to know about an
			// instance type that the site does not know about.
			var applicationErr *tp.ApplicationError
			if errors.As(wferr, &applicationErr) {
				if slices.Contains(swe.ObjectNotFoundErrTypes(), applicationErr.Type()) {
					logger.Warn().Msg(swe.ErrTypeNICoObjectNotFound + " received from Site")
					// Reset error to nil
					wferr = nil
				} else if slices.Contains(swe.UnimplementedOrDeniedErrTypes(), applicationErr.Type()) {
					// NICo _could_ respond with an unimplemented if it's an
					// older NICo that doesn't have the endpoint yet, but it's more
					// likely to respond with a PermissionDenied because the permission
					// for the path isn't there either, but we can watch for both just to
					// be safe.

					logger.Warn().Msg("NICo endpoint unimplemented or restricted response received from Site")
					// Reset error to nil because we'll want to ignore while
					// NICo is being rolled out.
					wferr = nil
				}
			}
		}

		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || wfCtx.Err() != nil {
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "InstanceType", "UpdateInstanceType")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "workflow timeout", nil)
			}

			code, uwerr := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(uwerr).Msg("failed to synchronously execute Temporal workflow to update InstanceType")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to update InstanceType on Site: %s", uwerr), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous update InstanceType workflow")
		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to update Instance Type, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	ait := model.NewAPIInstanceType(it, ssds, mcs, nil, nil)

	// Create response
	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, ait)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteInstanceTypeHandler is the API Handler for deleting a Site
type DeleteInstanceTypeHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteInstanceTypeHandler initializes and returns a new handler for deleting an Instance Type
func NewDeleteInstanceTypeHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) DeleteInstanceTypeHandler {
	return DeleteInstanceTypeHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Delete an Instance Type
// @Description Delete an Instance Type. Org's Infrastructure Provider must be associated with the Instance Type.
// @Tags instancetype
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Instance Type"
// @Success 204
// @Router /v2/org/{org}/nico/instance/type/{id} [delete]
func (dith DeleteInstanceTypeHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("InstanceType", "Delete", c)
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

	// Validate role, only Provider Admins are allowed to proceed from here
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Instance Type ID
	itStrID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("instancetype_id", itStrID))

	itID, err := uuid.Parse(itStrID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Instance Type ID in URL", nil)
	}

	// Check if org has an Infrastructure Provider
	ipDAO := cdbm.NewInfrastructureProviderDAO(dith.dbSession)

	ips, serr := ipDAO.GetAllByOrg(ctx, nil, org, nil)
	if serr != nil {
		logger.Error().Err(serr).Msg("error retrieving Infrastructure Provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to to retrieve Org entities to check Instance Type association", nil)
	}

	if len(ips) == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have an Infrastructure Provider", nil)
	}

	orgIP := &ips[0]

	// Get Instance Type
	itDAO := cdbm.NewInstanceTypeDAO(dith.dbSession)

	it, err := itDAO.GetByID(ctx, nil, itID, []string{"Site"})
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find InstanceType with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Instance Type from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instance Type", nil)
	}

	// Check if Instance Type is associated with the Org's Provider
	if orgIP.ID != it.InfrastructureProviderID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Instance Type is not associated with org's Infrastructure Provider", nil)
	}

	iDAO := cdbm.NewInstanceDAO(dith.dbSession)
	acDAO := cdbm.NewAllocationConstraintDAO(dith.dbSession)
	resourceType := cdbm.AllocationResourceTypeInstanceType

	var timeoutResp func() error

	err = cdb.WithTx(ctx, dith.dbSession, func(tx *cdb.Tx) error {
		// Acquire an advisory lock on this Instance Type so the read/check/delete
		// sequence below is serialized per Instance Type. Without this, another
		// request can attach this Instance Type between the usage checks and
		// DeleteByID — a TOCTOU window that would either turn into a generic
		// 500 from a DB constraint or, worse, delete a now-in-use record.
		derr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(it.ID.String()), nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to acquire advisory lock on Instance Type")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Instance Type", nil)
		}

		// Check if this Instance Type is being used. Done inside the locked tx
		// so a concurrent attach can't slip in between the check and the
		// delete.
		instances, _, derr := iDAO.GetAll(ctx, tx, cdbm.InstanceFilterInput{InstanceTypeIDs: []uuid.UUID{it.ID}}, cdbp.PageInput{}, nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Instances for Instance Type from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Instances for Instance Type", nil)
		}
		if len(instances) > 0 {
			return cutil.NewAPIError(http.StatusBadRequest, "Instance Type is being used by one or more Instances and cannot be deleted", nil)
		}

		acs, _, derr := acDAO.GetAll(ctx, tx, cdbm.AllocationConstraintFilterInput{
			ResourceType:    &resourceType,
			ResourceTypeIDs: []uuid.UUID{it.ID},
		}, cdbp.PageInput{}, nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Allocation Constraints for Instance Type from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Allocation Constraints for Instance Type", nil)
		}
		if len(acs) > 0 {
			logger.Warn().Msg("error deleting instance type as allocation constraints are present")
			return cutil.NewAPIError(http.StatusBadRequest, "Instance Type is being used by one or more Allocations and cannot be deleted", nil)
		}

		// Delete Machine/Instance Type associations
		mitDAO := cdbm.NewMachineInstanceTypeDAO(dith.dbSession)
		derr = mitDAO.DeleteAllByInstanceTypeID(ctx, tx, itID, false)
		if derr != nil {
			logger.Error().Err(derr).Msg("error deleting Machine/Instance Type associations for Instance Type")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Instance Type", nil)
		}

		mDAO := cdbm.NewMachineDAO(dith.dbSession)
		mcs, _, derr := mDAO.GetAll(ctx, tx, cdbm.MachineFilterInput{InstanceTypeIDs: []uuid.UUID{it.ID}, ExcludeMetadata: true}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Machines for Instance Type from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Machines for Instance Type", nil)
		}

		// Clear InstanceType from respective machines
		for _, mc := range mcs {
			_, derr := mDAO.Clear(ctx, tx, cdbm.MachineClearInput{MachineID: mc.ID, InstanceTypeID: true})
			if derr != nil {
				logger.Error().Err(derr).Msg("error clearing Machine associations for Instance Type")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to dissociate one or more Machines from Instance Type", nil)
			}
		}

		// Delete Instance Type
		derr = itDAO.DeleteByID(ctx, tx, itID)
		if derr != nil {
			logger.Error().Err(derr).Msg("error deleting Instance Type from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Instance Type", nil)
		}

		// Get the temporal client for the site we are working with.
		stc, derr := dith.scp.GetClientByID(*it.SiteID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		deleteInstanceTypeRequest := &corev1.DeleteInstanceTypeRequest{
			Id: it.ID.String(),
		}

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "instance-type-delete-" + it.ID.String(),
			TaskQueue:                queue.SiteTaskQueue,
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		}

		logger.Info().Msg("triggering InstanceType delete workflow")

		// Add context deadlines
		wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow
		we, wferr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "DeleteInstanceType", deleteInstanceTypeRequest)
		if wferr != nil {
			logger.Error().Err(wferr).Msg("failed to synchronously start Temporal workflow to delete InstanceType")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed start sync workflow to delete InstanceType on Site: %s", wferr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous delete InstanceType workflow")

		// Block until the workflow has completed and returned success/error.
		wferr = we.Get(wfCtx, nil)

		// Handle skippable errors
		if wferr != nil {
			// If this was a 404 back from NICo, we can treat the object as already having been deleted and allow things to proceed.
			var applicationErr *tp.ApplicationError
			if errors.As(wferr, &applicationErr) {
				if slices.Contains(swe.ObjectNotFoundErrTypes(), applicationErr.Type()) {
					logger.Warn().Msg(swe.ErrTypeNICoObjectNotFound + " received from Site")
					// Reset error to nil
					wferr = nil
				} else if slices.Contains(swe.UnimplementedOrDeniedErrTypes(), applicationErr.Type()) {
					// NICo _could_ respond with an unimplemented if it's an
					// older NICo that doesn't have the endpoint yet, but it's more
					// likely to respond with a PermissionDenied because the permission
					// for the path isn't there either, but we can watch for both just to
					// be safe.

					logger.Warn().Msg("NICo endpoint unimplemented or restricted response received from Site")
					// Reset error to nil because we'll want to ignore while
					// NICo is being rolled out.
					wferr = nil
				}
			}
		}

		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || wfCtx.Err() != nil {
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "InstanceType", "DeleteInstanceType")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "workflow timeout", nil)
			}

			code, uwerr := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(uwerr).Msg("failed to synchronously execute Temporal workflow to delete InstanceType")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to delete InstanceType on Site: %s", uwerr), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous delete InstanceType workflow")
		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to delete Instance Type, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// Create response
	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusAccepted, model.NewAPIDeletionAcceptedResponse())
}
