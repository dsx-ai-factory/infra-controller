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
	"strings"

	"go.opentelemetry.io/otel/attribute"
	temporalClient "go.temporal.io/sdk/client"
	tp "go.temporal.io/sdk/temporal"

	goset "github.com/deckarep/golang-set/v2"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"

	wfutil "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
)

// ~~~~~ Create Handler ~~~~~ //

// CreateNVLinkLogicalPartitionHandler is the API Handler for creating new NVLinkLogicalPartition
type CreateNVLinkLogicalPartitionHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewCreateNVLinkLogicalPartitionHandler initializes and returns a new handler for creating NVLinkLogicalPartition
func NewCreateNVLinkLogicalPartitionHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) CreateNVLinkLogicalPartitionHandler {
	return CreateNVLinkLogicalPartitionHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Create an NVLinkLogicalPartition
// @Description Create an NVLinkLogicalPartition
// @Tags NVLinkLogicalPartition
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APINVLinkLogicalPartitionCreateRequest true "NVLinkLogicalPartition creation request"
// @Success 201 {object} model.APINVLinkLogicalPartition
// @Router /v2/org/{org}/nico/nvlink-logical-partition [post]
func (cibph CreateNVLinkLogicalPartitionHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("NVLinkLogicalPartition", "Create", c)
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

	// Validate role, only Tenant Admins are allowed to create NVLinkLogicalPartition
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Validate request
	// Bind request data to API model
	apiRequest := model.APINVLinkLogicalPartitionCreateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating NVLink Logical Partition creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating NVLink Logical Partition request creation data", verr)
	}

	// Validate the tenant for which this NVLinkLogicalPartition is being created
	orgTenant, err := common.GetTenantForOrg(ctx, nil, cibph.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			logger.Warn().Err(err).Msg("Org does not have a Tenant associated")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("unable to retrieve tenant for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant for org", nil)
	}

	// Validate and Verify if Site is ready
	site, serr := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, cibph.dbSession)
	if serr != nil {
		if serr == common.ErrInvalidID {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create NVLink Logical Partition, Invalid Site ID: %s", apiRequest.SiteID), nil)
		}
		if serr == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Failed to create NVLink Logical Partition, Could not find Site with ID: %s ", apiRequest.SiteID), nil)
		}
		logger.Warn().Err(serr).Str("Site ID", apiRequest.SiteID).Msg("error retrieving Site from DB by ID")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create NVLink Logical Partition, Could not find Site with ID: %s, DB error", apiRequest.SiteID), nil)
	}

	if site.Status != cdbm.SiteStatusRegistered {
		logger.Warn().Msg(fmt.Sprintf("Unable to associate NVLink Logical Partition to Site: %s. Site is not in Registered state", site.ID.String()))
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create NVLink Logical Partition, Site: %s specified in request is not in Registered state", site.ID.String()), nil)
	}

	// Determine if tenant has access to requested site
	tsDAO := cdbm.NewTenantSiteDAO(cibph.dbSession)
	_, err = tsDAO.GetByTenantIDAndSiteID(ctx, nil, orgTenant.ID, site.ID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant is not associated with Site specified in query", nil)
		}
		logger.Warn().Err(err).Msg("error retrieving Tenant Site association from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to determine if Tenant has access to Site specified in query, DB error", nil)
	}

	// Check if site has NVLinkLogicalPartition enabled
	siteConfig := &cdbm.SiteConfig{}
	if site.Config != nil {
		siteConfig = site.Config
	}

	if !siteConfig.NVLinkPartition {
		logger.Warn().Msg(fmt.Sprintf("Site: %v specified in request data must have NVLink Logical Partition enabled in order to create NVLink Logical Partition", site.ID.String()))
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Site: %v specified in request data must have NVLink Logical Partition enabled in order to create NVLink Logical Partition", site.ID.String()), nil)
	}

	// check for name uniqueness for the tenant, ie, tenant cannot have another NVLinkLogicalPartition with same name
	// TODO consider doing this with an advisory lock for correctness
	nvllpDAO := cdbm.NewNVLinkLogicalPartitionDAO(cibph.dbSession)
	nvllps, tot, err := nvllpDAO.GetAll(
		ctx,
		nil,
		cdbm.NVLinkLogicalPartitionFilterInput{
			Names:     []string{apiRequest.Name},
			TenantIDs: []uuid.UUID{orgTenant.ID},
		},
		cdbp.PageInput{},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("db error checking for name uniqueness of tenant NVLink Logical Partition")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to check name uniqueness for NVLink Logical Partition, DB error", nil)
	}
	if tot > 0 {
		logger.Warn().Str("Tenant ID", orgTenant.ID.String()).Str("name", apiRequest.Name).Msg("NVLink Logical Partition with same name already exists for Tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Another NVLink Logical Partition with specified name already exists for Tenant", validation.Errors{
			"id": errors.New(nvllps[0].ID.String()),
		})
	}

	sdDAO := cdbm.NewStatusDetailDAO(cibph.dbSession)

	var nvllp *cdbm.NVLinkLogicalPartition
	var ssd *cdbm.StatusDetail
	var protoNvllp *corev1.NVLinkLogicalPartition
	// timeoutResp lets the closure signal a post-rollback handler — the
	// TerminateWorkflow call has to run after the closure returns so that
	// the DB tx unwinds before we make the second remote call. nil means
	// no timeout occurred and the normal flow continues.
	var timeoutResp func() error
	err = cdb.WithTx(ctx, cibph.dbSession, func(tx *cdb.Tx) error {
		// create the db record for NVLink Logical Partition
		var derr error
		nvllp, derr = nvllpDAO.Create(
			ctx,
			tx,
			cdbm.NVLinkLogicalPartitionCreateInput{
				Name:        apiRequest.Name,
				Description: apiRequest.Description,
				TenantOrg:   org,
				SiteID:      site.ID,
				TenantID:    orgTenant.ID,
				Status:      cdbm.NVLinkLogicalPartitionStatusPending,
				CreatedBy:   dbUser.ID,
			},
		)
		if derr != nil {
			logger.Error().Err(derr).Msg("unable to create NVLink Logical Partition record in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create NVLink Logical Partition, DB error", nil)
		}

		// create the status detail record
		ssd, derr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: nvllp.ID.String(), Status: string(cdbm.NVLinkLogicalPartitionStatusPending), Message: cutil.GetPtr("received NVLink Logical Partition creation request, pending")})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for NVLink Logical Partition", nil)
		}
		if ssd == nil {
			logger.Error().Msg("Status Detail DB entry not returned from Create")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to get new Status Detail for NVLink Logical Partition", nil)
		}

		// Get the temporal client for the site we are working with.
		stc, derr := cibph.scp.GetClientByID(site.ID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		createRequest := apiRequest.ToProto(nvllp)

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "nvlink-logical-partition-create-" + nvllp.ID.String(),
			TaskQueue:                queue.SiteTaskQueue,
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		}

		logger.Info().Msg("triggering NVLink Logical Partition creation")

		// Add context deadlines
		wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow
		we, derr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "CreateNVLinkLogicalPartition", createRequest)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to schedule NVLink Logical Partition creation workflow")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to schedule NVLink Logical Partition creation workflow on Site: %s", derr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("scheduled NVLink Logical Partition creation workflow")

		// Block until the workflow has completed and returned success/error.
		wferr := we.Get(wfCtx, &protoNvllp)
		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || wfCtx.Err() != nil {
				logger.Error().Err(wferr).Msg("failed to create NVLink Logical Partition, timeout occurred executing workflow on Site.")
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "NVLinkLogicalPartition", "Create")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "NVLink Logical Partition create workflow timed out", nil)
			}

			code, wferr := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(wferr).Msg("failed to create NVLink Logical Partition on Site")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to create NVLink Logical Partition on Site: %s", wferr), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed NVLink Logical Partition creation workflow")
		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to create NVLink Logical Partition, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// update the db record for NVLink Logical Partition with the status from the workflow
	// If we run into an error, we'll log it but won't return error
	unvllp := nvllp
	ssds := []cdbm.StatusDetail{*ssd}
	if protoNvllp != nil {
		logger.Info().Msg("received NVLink Logical Partition info from workflow")

		var status cdbm.NVLinkLogicalPartitionStatus
		status.FromProto(protoNvllp.Status.State)
		// if status is empty, then default is pending and inventory will be updating status from workflow
		if status != "" {
			message := status.Message()
			updatedNvllp, newSSD, err := wfutil.UpdateNVLinkLogicalPartitionStatusInDB(ctx, nil, cibph.dbSession, nvllp.ID, &status, &message)
			if err != nil {
				logger.Error().Err(err).Msg("failed to update NVLink Logical Partition status in DB")
			} else {
				if updatedNvllp != nil {
					unvllp = updatedNvllp
				}
				if newSSD != nil {
					ssds = append(ssds, *newSSD)
				}
			}
		}
	}

	// create response
	apiNVLinkLogicalPartition := model.NewAPINVLinkLogicalPartition(unvllp, nil, nil, ssds)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusCreated, apiNVLinkLogicalPartition)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllNVLinkLogicalPartitionHandler is the API Handler for getting all NVLinkLogicalPartitions
type GetAllNVLinkLogicalPartitionHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetAllNVLinkLogicalPartitionHandler initializes and returns a new handler for getting all NVLinkLogicalPartitions
func NewGetAllNVLinkLogicalPartitionHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllNVLinkLogicalPartitionHandler {
	return GetAllNVLinkLogicalPartitionHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get all NVLinkLogicalPartitions
// @Description Get all NVLinkLogicalPartitions
// @Tags NVLinkLogicalPartition
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of Site"
// @Param status query string false "Filter by status" e.g. 'Pending', 'Error'"
// @Param query query string false "Query input for full text search"
// @Param includeRelation query string false "Related entities to include in response e.g. 'InfrastructureProvider', 'Tenant'"
// @Param includeInterfaces query boolean false "Include NVLinkInterfaces in response"
// @Param includeStats query boolean false "Include NVLinkLogicalPartitionStats in response"
// @Param includeVpcs query boolean false "Include VPCs in response"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APINVLinkLogicalPartition
// @Router /v2/org/{org}/nico/nvlink-logical-partition [get]
func (gaibph GetAllNVLinkLogicalPartitionHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("NVLinkLogicalPartition", "GetAll", c)
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

	// Validate role, only Tenant Admins are allowed to retrieve NVLinkLogicalPartitions
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Validate pagination request
	pageRequest := pagination.PageRequest{}
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate request attributes
	err = pageRequest.Validate(cdbm.NVLinkLogicalPartitionOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Validate tenant for org
	tenant, err := common.GetTenantForOrg(ctx, nil, gaibph.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			logger.Warn().Err(err).Msg("Org does not have a Tenant associated")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("unable to retrieve tenant for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve tenant for org", nil)
	}

	// Get site ID from query param
	tsDAO := cdbm.NewTenantSiteDAO(gaibph.dbSession)
	var siteIDs []uuid.UUID
	siteIDStr := c.QueryParam("siteId")
	if siteIDStr != "" {
		site, err := common.GetSiteFromIDString(ctx, nil, siteIDStr, gaibph.dbSession)
		if err != nil {
			logger.Warn().Err(err).Msg("error getting site in request")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Site specified in query param, invalid ID or DB error", nil)
		}
		siteIDs = append(siteIDs, site.ID)

		// Check Site association with Tenant
		_, err = tsDAO.GetByTenantIDAndSiteID(ctx, nil, tenant.ID, site.ID, nil)
		if err != nil {
			if err == cdb.ErrDoesNotExist {
				return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have access to this Site", nil)
			}
			logger.Error().Err(err).Msg("error retrieving TenantSite from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to determine Tenant access to Site, DB error", nil)
		}
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.NVLinkLogicalPartitionRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Get query text for full text search from query param
	searchQuery := common.GetSearchQuery(c)
	if searchQuery != nil {
		cotel.SetAttribute(handlerSpan, attribute.String("query", *searchQuery))
	}

	// Get status from query param
	var statuses []string

	statusQuery := c.QueryParam("status")
	if statusQuery != "" {
		cotel.SetAttribute(handlerSpan, attribute.String("status", statusQuery))
		_, ok := cdbm.NVLinkLogicalPartitionStatusMap[cdbm.NVLinkLogicalPartitionStatus(statusQuery)]
		if !ok {
			logger.Warn().Msg(fmt.Sprintf("invalid value in status query: %v", statusQuery))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Status value in query", nil)
		}
		statuses = append(statuses, statusQuery)
	}

	// Check `includeInterfaces` in query
	includeInterfaces := false
	qin := c.QueryParam("includeInterfaces")
	if qin != "" {
		includeInterfaces, err = strconv.ParseBool(qin)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `includeInterfaces` query param", err)
		}
		cotel.SetAttribute(handlerSpan, attribute.Bool("includeInterfaces", includeInterfaces))
	}

	// Check `includeVpcs` in query
	includeVpcs := false
	qvp := c.QueryParam("includeVpcs")
	if qvp != "" {
		includeVpcs, err = strconv.ParseBool(qvp)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `includeVpcs` query param", err)
		}
		cotel.SetAttribute(handlerSpan, attribute.Bool("includeVpcs", includeVpcs))
	}

	// Check `includeStats` in query
	includeStats := false
	qinlps := c.QueryParam("includeStats")
	if qinlps != "" {
		includeStats, err = strconv.ParseBool(qinlps)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `includeStats` query param", nil)
		}
		cotel.SetAttribute(handlerSpan, attribute.Bool("includeStats", includeStats))
	}

	nvllpDAO := cdbm.NewNVLinkLogicalPartitionDAO(gaibph.dbSession)
	nvllps, total, err := nvllpDAO.GetAll(

		ctx,
		nil,
		cdbm.NVLinkLogicalPartitionFilterInput{
			SiteIDs:     siteIDs,
			TenantIDs:   []uuid.UUID{tenant.ID},
			Statuses:    statuses,
			SearchQuery: searchQuery,
		},
		cdbp.PageInput{Offset: pageRequest.Offset,
			Limit:   pageRequest.Limit,
			OrderBy: pageRequest.OrderBy,
		},
		qIncludeRelations,
	)
	if err != nil {
		logger.Error().Err(err).Msg("error getting NVLink Logical Partitions from db")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve NVLink Logical Partitions, DB error", nil)
	}

	nvllpIDs := []uuid.UUID{}
	for _, nvllp := range nvllps {
		nvllpIDs = append(nvllpIDs, nvllp.ID)
	}

	nvlifcMap := map[uuid.UUID][]cdbm.NVLinkInterface{}
	if includeInterfaces {
		nvlifcDAO := cdbm.NewNVLinkInterfaceDAO(gaibph.dbSession)
		dbnvlifcs, _, err := nvlifcDAO.GetAll(ctx, nil, cdbm.NVLinkInterfaceFilterInput{NVLinkLogicalPartitionIDs: nvllpIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, []string{})
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving NVLinkInterfaces from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve NVLink Interfaces for NVLink Logical Partitions, DB error", nil)
		}

		for _, nvlifc := range dbnvlifcs {
			curnvlifc := nvlifc
			nvlifcMap[curnvlifc.NVLinkLogicalPartitionID] = append(nvlifcMap[curnvlifc.NVLinkLogicalPartitionID], curnvlifc)
		}
	}

	vpcMap := map[uuid.UUID][]cdbm.Vpc{}
	if includeVpcs {
		vpcDAO := cdbm.NewVpcDAO(gaibph.dbSession)
		dbvpc, _, err := vpcDAO.GetAll(ctx, nil, cdbm.VpcFilterInput{NVLinkLogicalPartitionIDs: nvllpIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, []string{})
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving VPCs from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPCs for NVLink Logical Partitions, DB error", nil)
		}

		for _, vpc := range dbvpc {
			curnvpc := vpc
			if curnvpc.NVLinkLogicalPartitionID == nil {
				continue
			}
			vpcMap[*curnvpc.NVLinkLogicalPartitionID] = append(vpcMap[*curnvpc.NVLinkLogicalPartitionID], curnvpc)
		}
	}

	// Get NVLinkLogicalPartition stats if requested
	var nvllpStats map[uuid.UUID]*model.APINVLinkLogicalPartitionStats
	if includeStats {
		nvllpStats, err = common.GetNVLinkLogicalPartitionCountStats(ctx, nil, gaibph.dbSession, logger, nvllpIDs)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving NVLinkLogicalPartition stats from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve NVLink Logical Partition stats, DB error", nil)
		}
	}

	// Get status details
	sdDAO := cdbm.NewStatusDetailDAO(gaibph.dbSession)
	sdEntityIDs := []string{}
	for _, nvllp := range nvllps {
		sdEntityIDs = append(sdEntityIDs, nvllp.ID.String())
	}

	ssds, serr := sdDAO.GetRecentByEntityIDs(ctx, nil, sdEntityIDs, common.RECENT_STATUS_DETAIL_COUNT)
	if serr != nil {
		logger.Warn().Err(serr).Msg("error retrieving Status Details for NVLink Logical Partitions from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve status history for NVLink Logical Partitions, DB error", nil)
	}

	ssdMap := map[string][]cdbm.StatusDetail{}
	for _, ssd := range ssds {
		cssd := ssd
		ssdMap[ssd.EntityID] = append(ssdMap[ssd.EntityID], cssd)
	}

	// Create response
	apiNVLinkLogicalPartitions := []*model.APINVLinkLogicalPartition{}
	for _, nvllp := range nvllps {
		curnvllp := nvllp
		curnvllifcs, ok := nvlifcMap[nvllp.ID]
		if !ok {
			curnvllifcs = []cdbm.NVLinkInterface{}
		}
		curnvpc, ok := vpcMap[nvllp.ID]
		if !ok {
			curnvpc = []cdbm.Vpc{}
		}
		apiNVLinkLogicalPartition := model.NewAPINVLinkLogicalPartition(&curnvllp, curnvpc, curnvllifcs, ssdMap[nvllp.ID.String()])
		// Add NVLinkLogicalPartition stats if requested
		if includeStats {
			curnvllpStats, ok := nvllpStats[nvllp.ID]
			if !ok {
				curnvllpStats = model.NewAPINVLinkLogicalPartitionStats()
			}
			apiNVLinkLogicalPartition.NVLinkLogicalPartitionStats = curnvllpStats
		}

		apiNVLinkLogicalPartitions = append(apiNVLinkLogicalPartitions, apiNVLinkLogicalPartition)
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

	return c.JSON(http.StatusOK, apiNVLinkLogicalPartitions)
}

// ~~~~~ Get Handler ~~~~~ //

// GetNVLinkLogicalPartitionHandler is the API Handler for retrieving NVLinkLogicalPartition
type GetNVLinkLogicalPartitionHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetNVLinkLogicalPartitionHandler initializes and returns a new handler to retrieve NVLinkLogicalPartition
func NewGetNVLinkLogicalPartitionHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetNVLinkLogicalPartitionHandler {
	return GetNVLinkLogicalPartitionHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Retrieve the NVLinkLogicalPartition
// @Description Retrieve the NVLinkLogicalPartition
// @Tags NVLinkLogicalPartition
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of NVLinkLogicalPartition"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Site', 'Tenant'"
// @Param includeInterfaces query boolean false "Include NVLinkInterfaces in response"
// @Param includeStats query boolean false "Include NVLinkLogicalPartitionStats in response"
// @Success 200 {object} model.APINVLinkLogicalPartition
// @Router /v2/org/{org}/nico/nvlink-logical-partition/{id} [get]
func (gibph GetNVLinkLogicalPartitionHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("NVLinkLogicalPartition", "Get", c)
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

	// Validate role, only Tenant Admins are allowed to retrieve NVLinkLogicalPartition
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.NVLinkLogicalPartitionRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Check `includeInterfaces` in query
	includeInterfaces := false
	qin := c.QueryParam("includeInterfaces")
	if qin != "" {
		includeInterfaces, err = strconv.ParseBool(qin)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `includeInterfaces` query param", err)
		}
	}

	// Check `includeVpcs` in query
	includeVpcs := false
	qvp := c.QueryParam("includeVpcs")
	if qvp != "" {
		includeVpcs, err = strconv.ParseBool(qvp)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `includeVpcs` query param", err)
		}
	}

	// Check `includeStats` in query
	includeStats := false
	qinlps := c.QueryParam("includeStats")
	if qinlps != "" {
		includeStats, err = strconv.ParseBool(qinlps)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `includeStats` query param", err)
		}
		cotel.SetAttribute(handlerSpan, attribute.Bool("includeStats", includeStats))
	}

	// Get IB Partition ID from URL
	nvllpStrID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("nvlink_logical_partition_id", nvllpStrID))

	nvllpID, err := uuid.Parse(nvllpStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("invalid NVLink Logical Partition ID in URL")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "NVLink Logical Partition ID specified in URL is not valid", nil)
	}

	nvllpDAO := cdbm.NewNVLinkLogicalPartitionDAO(gibph.dbSession)

	// Validate the tenant for which this NVLinkLogicalPartition is being retrieved
	orgTenant, err := common.GetTenantForOrg(ctx, nil, gibph.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			logger.Warn().Err(err).Msg("Org does not have a Tenant associated")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("unable to retrieve Tenant for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant for org", nil)
	}

	// Check that NVLink Logical Partition exists
	nvllp, err := nvllpDAO.GetByID(ctx, nil, nvllpID, qIncludeRelations)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find NVLink Logical Partition with specified ID", nil)
		}

		logger.Error().Err(err).Msg("error retrieving NVLink Logical Partition from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Could not retrieve NVLink Logical Partition", nil)
	}

	if nvllp.TenantID != orgTenant.ID {
		logger.Warn().Msg("NVLink Logical Partition is not owned by current org's Tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "NVLink Logical Partition is not owned by current org's Tenant", nil)
	}

	var dbnvlifcs []cdbm.NVLinkInterface
	if includeInterfaces {
		nvlifcDAO := cdbm.NewNVLinkInterfaceDAO(gibph.dbSession)
		dbnvlifcs, _, err = nvlifcDAO.GetAll(ctx, nil, cdbm.NVLinkInterfaceFilterInput{NVLinkLogicalPartitionIDs: []uuid.UUID{nvllp.ID}}, cdbp.PageInput{}, []string{})
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving NVLink Interfaces from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve NVLink Interfaces for NVLink Logical Partition", nil)
		}
	}

	var dbvpc []cdbm.Vpc
	if includeVpcs {
		vpcDAO := cdbm.NewVpcDAO(gibph.dbSession)
		dbvpc, _, err = vpcDAO.GetAll(ctx, nil, cdbm.VpcFilterInput{NVLinkLogicalPartitionIDs: []uuid.UUID{nvllp.ID}}, cdbp.PageInput{}, []string{})
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving VPCs from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPCs for NVLink Logical Partition", nil)
		}
	}

	var nvllpStatsMap map[uuid.UUID]*model.APINVLinkLogicalPartitionStats
	if includeStats {
		nvllpStatsMap, err = common.GetNVLinkLogicalPartitionCountStats(ctx, nil, gibph.dbSession, logger, []uuid.UUID{nvllp.ID})
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving NVLinkLogicalPartition stats from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve NVLink Logical Partition stats, DB error", nil)
		}
	}

	// get status details for the response
	sdDAO := cdbm.NewStatusDetailDAO(gibph.dbSession)
	ssds, err := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{nvllp.ID.String()}, common.RECENT_STATUS_DETAIL_COUNT)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Status Details for NVLink Logical Partition from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Status Details for NVLink Logical Partition", nil)
	}

	// Send response
	apiNvllp := model.NewAPINVLinkLogicalPartition(nvllp, dbvpc, dbnvlifcs, ssds)

	// Add NVLinkLogicalPartition stats if requested
	if includeStats {
		curnvllpStats, ok := nvllpStatsMap[nvllp.ID]
		if !ok {
			curnvllpStats = model.NewAPINVLinkLogicalPartitionStats()
		}
		apiNvllp.NVLinkLogicalPartitionStats = curnvllpStats
	}

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiNvllp)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateNVLinkLogicalPartitionHandler is the API Handler for updating a NVLinkLogicalPartition
type UpdateNVLinkLogicalPartitionHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewUpdateNVLinkLogicalPartitionHandler initializes and returns a new handler for updating NVLinkLogicalPartition
func NewUpdateNVLinkLogicalPartitionHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) UpdateNVLinkLogicalPartitionHandler {
	return UpdateNVLinkLogicalPartitionHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Update an existing NVLinkLogicalPartition
// @Description Update an existing NVLinkLogicalPartition
// @Tags NVLinkLogicalPartition
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of NVLinkLogicalPartition"
// @Param message body model.APINVLinkLogicalPartitionUpdateRequest true "NVLinkLogicalPartition update request"
// @Success 200 {object} model.APINVLinkLogicalPartition
// @Router /v2/org/{org}/nico/nvlink-logical-partition/{id} [patch]
func (uibph UpdateNVLinkLogicalPartitionHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("NVLinkLogicalPartition", "Update", c)
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

	// Validate role, only Tenant Admins are allowed to update NVLinkLogicalPartition
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get IB Partition ID from URL
	nvllpStrID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("nvlink_logical_partition_id", nvllpStrID))

	nvllpID, err := uuid.Parse(nvllpStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid NVLink Logical Partition ID in URL", nil)
	}

	nvllpDAO := cdbm.NewNVLinkLogicalPartitionDAO(uibph.dbSession)

	// Validate request
	// Bind request data to API model
	apiRequest := model.APINVLinkLogicalPartitionUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating NVLink Logical Partition update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating NVLink Logical Partition update request data", verr)
	}

	// Validate the tenant for which this NVLinkLogical is being updated
	orgTenant, err := common.GetTenantForOrg(ctx, nil, uibph.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			logger.Warn().Err(err).Msg("Org does not have a Tenant associated")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("unable to retrieve Tenant for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant for org", nil)
	}

	// check that NVLinkLogical exists
	nvllp, err := nvllpDAO.GetByID(ctx, nil, nvllpID, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving NVLink Logical Partition DB entity")
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find NVLink Logical Partition with ID specified in URL", nil)
		}
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Could not retrieve NVLink Logical Partition to update", nil)
	}

	// verify tenant matches
	if nvllp.TenantID != orgTenant.ID {
		logger.Warn().Msg("NVLink Logical Partition is not owned by current org's Tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "NVLink Logical Partition is not owned by current org's Tenant", nil)
	}

	needsUpdate := false
	if apiRequest.Name != nil && *apiRequest.Name != nvllp.Name {
		needsUpdate = true
	}

	if apiRequest.Description != nil && (nvllp.Description == nil || *apiRequest.Description != *nvllp.Description) {
		needsUpdate = true
	}

	// get status details for the response
	sdDAO := cdbm.NewStatusDetailDAO(uibph.dbSession)
	ssds, _, err := sdDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{nvllp.ID.String()}}, cdbp.PageInput{Limit: cutil.GetPtr(pagination.MaxPageSize)})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Status Details for NVLink Logical Partition from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Status Details for NVLink Logical Partition", nil)
	}

	if !needsUpdate {
		// no updates needed, send response
		apiNvllp := model.NewAPINVLinkLogicalPartition(nvllp, nil, nil, ssds)
		logger.Info().Msg("finishing API handler")
		return c.JSON(http.StatusOK, apiNvllp)
	}

	// check for name uniqueness for the tenant, ie, tenant cannot have another NVLink Logical Partition with same name
	if apiRequest.Name != nil && *apiRequest.Name != nvllp.Name {
		nvllps, tot, serr := nvllpDAO.GetAll(
			ctx,
			nil,
			cdbm.NVLinkLogicalPartitionFilterInput{
				Names:     []string{*apiRequest.Name},
				TenantIDs: []uuid.UUID{orgTenant.ID},
			},
			cdbp.PageInput{},
			nil,
		)
		if serr != nil {
			logger.Error().Err(serr).Msg("db error checking for name uniqueness of tenant's NVLink Logical Partition")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to update NVLink Logical Partition due to DB error", nil)
		}
		if tot > 0 {
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Another NVLink Logical Partition with specified name already exists for Tenant", validation.Errors{
				"id": errors.New(nvllps[0].ID.String()),
			})
		}
	}

	// timeoutResp lets the closure signal a post-rollback handler — the
	// TerminateWorkflow call has to run after the closure returns so that
	// the DB tx unwinds before we make the second remote call. nil means
	// no timeout occurred and the normal flow continues.
	var timeoutResp func() error
	unvllp, err := cdb.WithTxResult(ctx, uibph.dbSession, func(tx *cdb.Tx) (*cdbm.NVLinkLogicalPartition, error) {
		updated, derr := nvllpDAO.Update(
			ctx,
			tx,
			cdbm.NVLinkLogicalPartitionUpdateInput{
				NVLinkLogicalPartitionID: nvllpID,
				Name:                     apiRequest.Name,
				Description:              apiRequest.Description,
			},
		)
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating NVLink Logical Partition in DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to update NVLink Logical Partition", nil)
		}
		logger.Info().Msg("done updating NVLink Logical Partition in DB")

		// Get the Temporal client for the site we are working with
		stc, derr := uibph.scp.GetClientByID(updated.SiteID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		// Site Controller (NICo) requires metadata.name on every update. When the
		// client sends only description, apiRequest.Name is nil but we must still
		// send the current partition name from the DB; ToProto reads it directly
		// off the (already-updated) DB entity via the entity's ToProto().
		updateRequest := apiRequest.ToProto(updated)

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "nvlink-logical-partition-update-" + updated.ID.String(),
			TaskQueue:                queue.SiteTaskQueue,
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		}

		logger.Info().Msg("triggering NVLink Logical Partition update")

		// Add context deadlines
		wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow
		we, derr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "UpdateNVLinkLogicalPartition", updateRequest)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to schedule NVLink Logical Partition update workflow")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to schedule NVLink Logical Partition update on Site: %s", derr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("scheduled NVLink Logical Partition update workflow")

		// Block until the workflow has completed and returned success/error.
		wferr := we.Get(wfCtx, nil)
		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || wfCtx.Err() != nil {
				logger.Error().Err(wferr).Msg("failed to update NVLink Logical Partition, timeout occurred executing workflow on Site.")
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "NVLinkLogicalPartition", "Update")
				}
				return nil, cutil.NewAPIError(http.StatusInternalServerError, "NVLink Logical Partition update workflow timed out", nil)
			}

			code, wferr := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(wferr).Msg("failed to execute NVLink Logical Partition update workflow")
			return nil, cutil.NewAPIError(code, fmt.Sprintf("Failed to update NVLink Logical Partition on Site: %s", wferr), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed NVLink Logical Partition update workflow")
		return updated, nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to update NVLink Logical Partition, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// send response
	apiNvllp := model.NewAPINVLinkLogicalPartition(unvllp, nil, nil, ssds)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiNvllp)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteNVLinkLogicalPartitionHandler is the API Handler for deleting a NVLinkLogicalPartition
type DeleteNVLinkLogicalPartitionHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteNVLinkLogicalPartitionHandler initializes and returns a new handler for deleting NVLinkLogicalPartition
func NewDeleteNVLinkLogicalPartitionHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) DeleteNVLinkLogicalPartitionHandler {
	return DeleteNVLinkLogicalPartitionHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Delete an existing NVLinkLogicalPartition
// @Description Delete an existing NVLinkLogicalPartition
// @Tags NVLinkLogicalPartition
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of NVLinkLogicalPartition"
// @Success 202
// @Router /v2/org/{org}/nico/nvlink-logical-partition/{id} [delete]
func (dibph DeleteNVLinkLogicalPartitionHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("NVLinkLogicalPartition", "Delete", c)
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

	// Validate role, only Tenant Admins are allowed to delete NVLinkLogicalPartition
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get NVLink Logical Partition ID from URL param
	nvllpStrID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("nvlink_logical_partition_id", nvllpStrID))

	nvllpID, err := uuid.Parse(nvllpStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid NVLink Logical Partition ID in URL", nil)
	}

	// Validate the tenant for which this NVLinkLogical is being updated
	orgTenant, err := common.GetTenantForOrg(ctx, nil, dibph.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			logger.Warn().Err(err).Msg("Org does not have a Tenant associated")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("unable to retrieve Tenant for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant for org", nil)
	}

	// Check that NVLink Logical Partition exists
	nvllpDAO := cdbm.NewNVLinkLogicalPartitionDAO(dibph.dbSession)
	nvllp, err := nvllpDAO.GetByID(ctx, nil, nvllpID, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving NVLink Logical Partition DB entity")
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not retrieve NVLink Logical Partition to delete", nil)
		}
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Could not retrieve NVLink Logical Partition to delete", nil)
	}

	// verify tenant matches
	if nvllp.TenantID != orgTenant.ID {
		logger.Warn().Msg("NVLink Logical Partition is not owned by current org's Tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "NVLink Logical Partition is not owned by current org's Tenant", nil)
	}

	// Verify that the NVLink Logical Partition is not being used by any VPC
	vpcDAO := cdbm.NewVpcDAO(dibph.dbSession)
	vpcFilter := cdbm.VpcFilterInput{
		TenantIDs:                 []uuid.UUID{orgTenant.ID},
		NVLinkLogicalPartitionIDs: []uuid.UUID{nvllpID},
	}
	vpcs, _, err := vpcDAO.GetAll(ctx, nil, vpcFilter, cdbp.PageInput{}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving VPCs from DB for NVLink Logical Partition")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPCs for NVLink Logical Partition", nil)
	}

	var vpcIDs []string
	for _, vpc := range vpcs {
		vpcIDs = append(vpcIDs, vpc.ID.String())
	}

	if len(vpcs) > 0 {
		logger.Warn().Msg("NVLink Logical Partition is being used by one or more VPCs")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "NVLink Logical Partition is being used by one or more VPCs", validation.Errors{"vpcIds": errors.New(strings.Join(vpcIDs, ", "))})

	}

	// Block deletion while Instances referenced by NVLink Interfaces still exist in the DB
	nvlifcDAO := cdbm.NewNVLinkInterfaceDAO(dibph.dbSession)
	nvInterfaces, _, err := nvlifcDAO.GetAll(ctx, nil, cdbm.NVLinkInterfaceFilterInput{
		NVLinkLogicalPartitionIDs: []uuid.UUID{nvllpID},
	}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving NVLink Interfaces from DB for NVLink Logical Partition")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve NVLink Interfaces for NVLink Logical Partition", nil)
	}

	instanceIDSet := goset.NewSet[uuid.UUID]()
	for _, nvlifc := range nvInterfaces {
		instanceIDSet.Add(nvlifc.InstanceID)
	}

	if instanceIDSet.Cardinality() > 0 {
		instanceDAO := cdbm.NewInstanceDAO(dibph.dbSession)
		activeCount, err := instanceDAO.GetCount(ctx, nil, cdbm.InstanceFilterInput{
			InstanceIDs: instanceIDSet.ToSlice(),
		})
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving count of Instances from DB for NVLink Logical Partition interface check")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve count of Instances for NVLink Logical Partition", nil)
		}
		if activeCount > 0 {
			logger.Warn().Int("active_instance_count", activeCount).Msg("NVLink Logical Partition has active Instances associated via interfaces")
			msg := fmt.Sprintf("%d active Instances are associated with this NVLink Logical Partition, unable to delete", activeCount)
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, msg, nil)
		}
	}

	sdDAO := cdbm.NewStatusDetailDAO(dibph.dbSession)

	// timeoutResp lets the closure signal a post-rollback handler — the
	// TerminateWorkflow call has to run after the closure returns so that
	// the DB tx unwinds before we make the second remote call. nil means
	// no timeout occurred and the normal flow continues.
	var timeoutResp func() error
	err = cdb.WithTx(ctx, dibph.dbSession, func(tx *cdb.Tx) error {
		// Update NVLink Logical Partition and set status to Deleting
		deletingStatus := cdbm.NVLinkLogicalPartitionStatusDeleting
		if _, derr := nvllpDAO.Update(
			ctx,
			tx,
			cdbm.NVLinkLogicalPartitionUpdateInput{
				NVLinkLogicalPartitionID: nvllpID,
				Status:                   &deletingStatus,
			},
		); derr != nil {
			logger.Error().Err(derr).Msg("error updating NVLink Logical Partition in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete NVLink Logical Partition, DB error", nil)
		}

		// Create status detail
		ssd, derr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: nvllp.ID.String(), Status: string(deletingStatus), Message: cutil.GetPtr("Received request for deletion, pending processing")})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for NVLink Logical Partition deletion", nil)
		}
		if ssd == nil {
			logger.Error().Msg("Status Detail DB entry not returned from Create")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for NVLink Logical Partition deletion", nil)
		}

		// Get the temporal client for the site we are working with.
		stc, derr := dibph.scp.GetClientByID(nvllp.SiteID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		deleteNvllpRequest := nvllp.ToDeletionRequestProto()

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "nvlink-logical-partition-delete-" + nvllp.ID.String(),
			TaskQueue:                queue.SiteTaskQueue,
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		}

		logger.Info().Msg("triggering NVLink Logical Partition deletion workflow")

		// Add context deadlines
		wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow
		we, derr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "DeleteNVLinkLogicalPartition", deleteNvllpRequest)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to schedule NVLink Logical Partition deletion workflow")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to schedule NVLink Logical Partition deletion on Site: %s", derr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("scheduled NVLink Logical Partition deletion workflow")

		// Execute the workflow synchronously
		wferr := we.Get(wfCtx, nil)
		// Handle skippable errors
		if wferr != nil {
			// If this was a 404 back from NICo, we can treat the object as already having been deleted and allow things to proceed.
			var applicationErr *tp.ApplicationError
			if errors.As(wferr, &applicationErr) && slices.Contains(swe.ObjectNotFoundErrTypes(), applicationErr.Type()) {
				logger.Warn().Msg(swe.ErrTypeNICoObjectNotFound + " received from Site")
				// Reset error to nil
				wferr = nil
			}
		}

		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || wfCtx.Err() != nil {
				logger.Error().Err(wferr).Msg("failed to delete NVLink Logical Partition, timeout occurred executing workflow on Site.")
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "NVLinkLogicalPartition", "Delete")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "NVLink Logical Partition delete workflow timed out", nil)
			}

			code, wferr := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(wferr).Msg("failed to execute Temporal workflow to delete NVLink Logical Partition")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to delete NVLink Logical Partition on Site: %s", wferr), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed NVLink Logical Partition deletion workflow")
		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to delete NVLink Logical Partition, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// Create response
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusAccepted, model.NewAPIDeletionAcceptedResponse())
}
