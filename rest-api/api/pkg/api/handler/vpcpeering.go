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

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"go.opentelemetry.io/otel/attribute"
	tclient "go.temporal.io/sdk/client"
	tp "go.temporal.io/sdk/temporal"

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
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
)

// ~~~~~ Create VPC Peering Handler ~~~~~ //

// CreateVpcPeeringHandler is the API Handler for creating new VPC Peering
type CreateVpcPeeringHandler struct {
	dbSession *cdb.Session
	tc        tclient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewCreateVpcPeeringHandler initializes and returns a new handler for creating VPC Peering
func NewCreateVpcPeeringHandler(dbSession *cdb.Session, tc tclient.Client, sc *sc.ClientPool, cfg *config.Config) CreateVpcPeeringHandler {
	return CreateVpcPeeringHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       sc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Create a VPC Peering
// @Description Create a VPC Peering between two VPCs on the same site.
// @Tags vpcpeering
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIVpcPeeringCreateRequest true "VPC Peering create request"
// @Success 201 {object} model.APIVpcPeering
// @Router /v2/org/{org}/nico/vpc-peering [post]
func (cvph CreateVpcPeeringHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Create", "VpcPeering", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Ensure our user is a provider or tenant for the org
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, cvph.dbSession, org, dbUser, false, nil)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIVpcPeeringCreateRequest{}
	err := c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating VPC Peering creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating VPC Peering creation request data", verr)
	}

	// Retrieve the Site from the DB
	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, cvph.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request data does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request data, DB error", nil)
	}

	// Validate the Site is in Registered state
	if site.Status != cdbm.SiteStatusRegistered {
		logger.Warn().Msg("Site specified in request data is not in Registered state")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request data is not in Registered state, cannot create VPC Peering", nil)
	}

	// Parse VPC IDs from the request body
	vpc1ID := uuid.MustParse(apiRequest.Vpc1ID)
	vpc2ID := uuid.MustParse(apiRequest.Vpc2ID)

	// Validate both VPCs exist
	vpcDAO := cdbm.NewVpcDAO(cvph.dbSession)
	vpc1, err := vpcDAO.GetByID(ctx, nil, vpc1ID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find VPC with ID: %s", vpc1ID.String()), nil)
		}
		logger.Error().Err(err).Msg("error retrieving VPC 1 from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to retrieve VPC 1 with ID: %s, DB error", vpc1ID.String()), nil)
	}
	vpc2, err := vpcDAO.GetByID(ctx, nil, vpc2ID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find VPC with ID: %s", vpc2ID.String()), nil)
		}
		logger.Error().Err(err).Msg("error retrieving VPC 2 from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to retrieve VPC 2 with ID: %s, DB error", vpc2ID.String()), nil)
	}

	// Validate VPCs are both on the provided site
	if vpc1.SiteID != site.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC 1: %s does not belong to Site: %s", vpc1ID.String(), site.ID.String()), nil)
	} else if vpc2.SiteID != site.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC 2: %s does not belong to Site: %s", vpc2ID.String(), site.ID.String()), nil)
	}

	// Validate VPCs are in Ready state
	if vpc1.Status != cdbm.VpcStatusReady || vpc2.Status != cdbm.VpcStatusReady {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Both VPCs must be in Ready state to proceed with peering", nil)
	}

	// Determine if the two VPCs belong to different tenants
	isMultiTenant := vpc1.TenantID != vpc2.TenantID

	// Validate user is authorized to create the VPC Peering
	providerAuthorized := false
	if infrastructureProvider != nil {
		// Provider Admin creating multi-tenant peerings requires both tenants to have
		// Tenant Accounts with the Provider and both tenants must have access to the Site.
		if site.InfrastructureProviderID == infrastructureProvider.ID && isMultiTenant {
			taDAO := cdbm.NewTenantAccountDAO(cvph.dbSession)
			_, taCount, err := taDAO.GetAll(ctx, nil, cdbm.TenantAccountFilterInput{
				InfrastructureProviderID: &infrastructureProvider.ID,
				TenantIDs:                []uuid.UUID{vpc1.TenantID, vpc2.TenantID},
			}, cdbp.PageInput{}, nil)
			if err != nil {
				logger.Error().Err(err).Msg("error retrieving Tenant Accounts for tenants of the VPC Peering")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to validate Tenant Accounts for tenants of the VPC Peering, DB error", nil)
			}
			if taCount == 2 {
				tsDAO := cdbm.NewTenantSiteDAO(cvph.dbSession)
				tenantSites, _, serr := tsDAO.GetAll(
					ctx,
					nil,
					cdbm.TenantSiteFilterInput{
						TenantIDs: []uuid.UUID{vpc1.TenantID, vpc2.TenantID},
						SiteIDs:   []uuid.UUID{site.ID},
					},
					cdbp.PageInput{},
					nil,
				)
				if serr != nil {
					logger.Error().Err(serr).Msg("error retrieving TenantSite for tenants of the VPC Peering")
					return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to validate Site access for tenants of the VPC Peering, DB error", nil)
				}
				if len(tenantSites) == 2 {
					providerAuthorized = true
				} else {
					logger.Warn().Msg("Not all tenants have access to Site specified in request")
				}
			} else {
				logger.Warn().Msg("Not all tenants have Tenant Accounts with the Provider")
			}
		}
	}

	tenantAuthorized := false
	if tenant != nil && !providerAuthorized {
		// Tenant Admin: tenant must have access to the site.
		tsDAO := cdbm.NewTenantSiteDAO(cvph.dbSession)
		tenantSites, _, err := tsDAO.GetAll(
			ctx,
			nil,
			cdbm.TenantSiteFilterInput{
				TenantIDs: []uuid.UUID{tenant.ID},
				SiteIDs:   []uuid.UUID{site.ID},
			},
			paginator.PageInput{Limit: cutil.GetPtr(1)},
			nil,
		)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving TenantSite from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to validate Site access for Tenant, DB error", nil)
		}

		// Tenant-only users can only create single-tenant peerings where both VPCs belong to them.
		if len(tenantSites) > 0 && vpc1.TenantID == tenant.ID && vpc2.TenantID == tenant.ID {
			tenantAuthorized = true
		} else {
			if vpc1.TenantID != tenant.ID {
				logger.Warn().Str("vpc_id", vpc1.ID.String()).Str("tenant_id", tenant.ID.String()).Msg("VPC 1 does not belong to Tenant associated with current org")
			}
			if vpc2.TenantID != tenant.ID {
				logger.Warn().Str("vpc_id", vpc2.ID.String()).Str("tenant_id", tenant.ID.String()).Msg("VPC 2 does not belong to Tenant associated with current org")
			}
		}
	}

	if !providerAuthorized && !tenantAuthorized {
		logger.Warn().Msg("User does not have access to create the VPC Peering")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have access to create the VPC Peering", nil)
	}

	// Check if peering already exists
	vpcPeeringDAO := cdbm.NewVpcPeeringDAO(cvph.dbSession)
	existingPeerings, _, err := vpcPeeringDAO.GetAll(ctx, nil, cdbm.VpcPeeringFilterInput{
		VpcIDs: []uuid.UUID{vpc1ID},
	}, cdbp.PageInput{}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error checking for existing VPC Peering")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to check for existing VPC Peering, DB error", nil)
	}
	for _, peering := range existingPeerings {
		// One of the VPC IDs must match the VPC ID, so we only need to check if either one equals the peer VPC ID.
		if peering.Vpc1ID == vpc2ID || peering.Vpc2ID == vpc2ID {
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "VPC Peering already exists between VPCs specified in request data", nil)
		}
	}

	var infrastructureProviderID *uuid.UUID
	if providerAuthorized && infrastructureProvider != nil {
		infrastructureProviderID = &infrastructureProvider.ID
	}

	var tenantID *uuid.UUID
	if tenantAuthorized && tenant != nil {
		tenantID = &tenant.ID
	}

	sdDAO := cdbm.NewStatusDetailDAO(cvph.dbSession)

	// vpcPeering is populated inside the closure and consumed by the
	// best-effort post-commit status update and the response payload.
	var vpcPeering *cdbm.VpcPeering

	// timeoutResp lets the closure signal a post-rollback handler — the
	// TerminateWorkflow call has to run after the closure returns so that
	// the DB tx unwinds before we make the second remote call. nil means
	// no timeout occurred and the normal flow continues.
	var timeoutResp func() error
	err = cdb.WithTx(ctx, cvph.dbSession, func(tx *cdb.Tx) error {
		// Create the VPC Peering in db
		createdVpcPeering, derr := vpcPeeringDAO.Create(
			ctx,
			tx,
			cdbm.VpcPeeringCreateInput{
				Vpc1ID:                   vpc1ID,
				Vpc2ID:                   vpc2ID,
				SiteID:                   site.ID,
				IsMultiTenant:            isMultiTenant,
				InfrastructureProviderID: infrastructureProviderID,
				TenantID:                 tenantID,
				CreatedByID:              dbUser.ID,
			},
		)
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating VPC Peering record in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create VPC Peering, DB error", nil)
		}
		vpcPeering = createdVpcPeering

		// Create a status detail record for the VPC Peering
		statusDetail, derr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: vpcPeering.ID.String(), Status: *cutil.GetPtr(cdbm.VpcPeeringStatusPending), Message: cutil.GetPtr("Received VPC Peering creation request, pending processing")})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating status detail for VPC Peering")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for VPC Peering", nil)
		}
		if statusDetail == nil {
			logger.Error().Msg("Status Detail DB entry not returned from Create")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to get new Status Detail for VPC Peering", nil)
		}

		// Create the peering directly in NICo via site agent
		derr = vpcPeeringDAO.UpdateStatusByID(ctx, tx, vpcPeering.ID, cdbm.VpcPeeringStatusConfiguring)
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating VPC Peering status to Configuring")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update VPC Peering status to Configuring", nil)
		}

		// Create the VPC Peering creation request. The entity's
		// `ToProto()` is the source of the canonical wire fields; the API
		// request's `ToProto` produces the request-shape envelope around
		// it.
		createVpcPeeringRequest := apiRequest.ToProto(vpcPeering)

		logger.Info().Msg("triggering VPC Peering create workflow on Site")

		// Create workflow options
		workflowOptions := tclient.StartWorkflowOptions{
			ID:                       "vpcpeering-create-" + vpcPeering.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		// Get the temporal client for the site we are working with
		stc, derr := cvph.scp.GetClientByID(vpcPeering.SiteID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Temporal client for Site", nil)
		}

		// Add context deadline
		workflowCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow
		workflowRun, derr := stc.ExecuteWorkflow(workflowCtx, workflowOptions, "CreateVpcPeering", createVpcPeeringRequest)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to start VPC Peering creation workflow")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to start VPC Peering creation workflow", nil)
		}

		workflowId := workflowRun.GetID()

		logger.Info().Str("Workflow ID", workflowId).Msg("started VPC Peering creation workflow")

		// Wait for workflow completion synchronously
		wferr := workflowRun.Get(workflowCtx, nil)
		if wferr != nil {
			var applicationErr *tp.ApplicationError
			if errors.As(wferr, &applicationErr) && slices.Contains(swe.UnimplementedOrDeniedErrTypes(), applicationErr.Type()) {
				logger.Error().Msg("feature not yet implemented on target Site")
				return cutil.NewAPIError(http.StatusNotImplemented, fmt.Sprintf("Feature not yet implemented on target Site: %s", wferr), nil)
			}

			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || workflowCtx.Err() != nil {
				logger.Error().Err(wferr).Msg("failed to create VPC Peering, timeout occurred executing workflow on Site.")
				timeoutCause := wferr
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, workflowId, timeoutCause, "VpcPeering", "CreateVpcPeering")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "VPC Peering create workflow timed out", nil)
			}

			logger.Error().Err(wferr).Msg("failed to synchronously execute Temporal workflow to update CreateVpcPeering")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to execute sync workflow to create VPC Peering on Site: %s", wferr), nil)
		}

		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to create VPC Peering, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// Best effort post-commit update: workflow completed, so mark peering as Ready.
	// This is intentionally outside of the transaction so create does not fail if this update fails.
	status := cdbm.VpcPeeringStatusConfiguring
	uerr := vpcPeeringDAO.UpdateStatusByID(ctx, nil, vpcPeering.ID, cdbm.VpcPeeringStatusReady)
	if uerr != nil {
		logger.Warn().Err(uerr).Msg("best-effort update to Ready status failed after workflow completion")
	} else {
		status = cdbm.VpcPeeringStatusReady
	}

	// Update API model with best-known status.
	apiVpcPeering := model.NewAPIVpcPeering(*vpcPeering, nil)
	apiVpcPeering.Status = status

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusCreated, apiVpcPeering)
}

// ~~~~~ Get All VPC Peering Handler ~~~~~ //

// GetAllVpcPeeringHandler is the API Handler for getting all VPC Peerings
type GetAllVpcPeeringHandler struct {
	dbSession *cdb.Session
	tc        tclient.Client
	cfg       *config.Config
}

// NewGetAllVpcPeeringHandler initializes and returns a new handler for getting all VPC Peerings
func NewGetAllVpcPeeringHandler(dbSession *cdb.Session, tc tclient.Client, cfg *config.Config) GetAllVpcPeeringHandler {
	return GetAllVpcPeeringHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get all VPC Peerings
// @Description Get all VPC Peerings visible to the user.
// @Tags vpcpeering
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string false "Filter by Site ID"
// @Param isMultiTenant query bool false "Filter by single-tenant or multi-tenant peerings"
// @Param status query string false "Filter by status (repeatable for multiple values)"
// @Param vpcId query string false "Filter by VPC ID involved in the peering (as vpc1 or vpc2)"
// @Param peerTenantId query string false "Filter by tenant ID of a VPC involved in the peering (repeatable for multiple values)"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Vpc1', 'Vpc2', 'Site'"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {array} model.APIVpcPeering
// @Router /v2/org/{org}/nico/vpc-peering [get]
func (gavph GetAllVpcPeeringHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("GetAll", "VpcPeering", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Ensure our user is a provider or tenant for the org
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, gavph.dbSession, org, dbUser, false, nil)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	filterInput := cdbm.VpcPeeringFilterInput{}

	qParams := c.QueryParams()

	// Get Site ID from query param if specified and verify user has access to the Site
	if siteIDStrs := qParams["siteId"]; len(siteIDStrs) > 0 {
		siteIDStr := siteIDStrs[0]
		providerSiteAuthorized := false
		tenantSiteAuthorized := false

		site, err := common.GetSiteFromIDString(ctx, nil, siteIDStr, gavph.dbSession)
		if err != nil {
			if errors.Is(err, common.ErrInvalidID) {
				logger.Warn().Msg(fmt.Sprintf("invalid value in siteId query: %v", siteIDStr))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid Site ID %v in query", siteIDStr), nil)
			}
			if errors.Is(err, cdb.ErrDoesNotExist) {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in query does not exist", nil)
			}
			logger.Error().Err(err).Msg("error retrieving Site from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in query, DB error", nil)
		}

		if infrastructureProvider != nil {
			providerSiteAuthorized = site.InfrastructureProviderID == infrastructureProvider.ID
		}

		if !providerSiteAuthorized && tenant != nil {
			tsDAO := cdbm.NewTenantSiteDAO(gavph.dbSession)
			tenantSites, _, err := tsDAO.GetAll(
				ctx,
				nil,
				cdbm.TenantSiteFilterInput{
					TenantIDs: []uuid.UUID{tenant.ID},
					SiteIDs:   []uuid.UUID{site.ID},
				},
				paginator.PageInput{Limit: cutil.GetPtr(1)},
				nil,
			)
			if err != nil {
				logger.Error().Err(err).Msg("error retrieving TenantSite from DB")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to validate Site access for Tenant, DB error", nil)
			}
			tenantSiteAuthorized = len(tenantSites) > 0
		}

		if !providerSiteAuthorized && !tenantSiteAuthorized {
			logger.Warn().Msg("User does not have access to Site specified in query")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have access to Site specified in query", nil)
		}

		filterInput.SiteIDs = []uuid.UUID{site.ID}
		cotel.SetAttribute(handlerSpan, attribute.String("site_id", siteIDStr))
	}

	// Get isMultiTenant from query param if specified
	var isMultiTenant *bool
	if isMultiTenantStrs := qParams["isMultiTenant"]; len(isMultiTenantStrs) > 0 {
		isMultiTenantParam, err := strconv.ParseBool(isMultiTenantStrs[0])
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `isMultiTenant` query param", nil)
		}
		isMultiTenant = &isMultiTenantParam
	}

	// Get and validate includeRelation params
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.VpcPeeringRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Get status from query param
	qStatuses := qParams["status"]
	if len(qStatuses) > 0 {
		cotel.SetAttribute(handlerSpan, attribute.StringSlice("status", qStatuses))
		for _, status := range qStatuses {
			if !cdbm.VpcPeeringStatusMap[status] {
				logger.Warn().Msg(fmt.Sprintf("invalid value in status query: %v", status))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid Status value %v in query", status), nil)
			}
			filterInput.Statuses = append(filterInput.Statuses, status)
		}
	}

	// Get vpcId from query param
	vpcIDStrs := qParams["vpcId"]
	if len(vpcIDStrs) > 0 {
		cotel.SetAttribute(handlerSpan, attribute.StringSlice("vpcId", vpcIDStrs))
		vpcIDs := make([]uuid.UUID, 0, len(vpcIDStrs))
		for _, vpcIDStr := range vpcIDStrs {
			vpcID, err := uuid.Parse(vpcIDStr)
			if err != nil {
				logger.Warn().Msg(fmt.Sprintf("invalid value in vpcId query: %v", vpcIDStr))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid VPC ID %v in query", vpcIDStr), nil)
			}
			vpcIDs = append(vpcIDs, vpcID)
		}
		vpcDAO := cdbm.NewVpcDAO(gavph.dbSession)
		vpcs, _, err := vpcDAO.GetAll(
			ctx,
			nil,
			cdbm.VpcFilterInput{VpcIDs: vpcIDs},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving VPCs from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPCs specified in query", nil)
		}
		vpcIDsMap := make(map[uuid.UUID]struct{}, len(vpcs))
		for _, vpc := range vpcs {
			vpcIDsMap[vpc.ID] = struct{}{}
		}
		for _, vpcID := range vpcIDs {
			if _, ok := vpcIDsMap[vpcID]; !ok {
				logger.Warn().Msg(fmt.Sprintf("could not find VPC with ID %v specified in query", vpcID))
				return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find VPC with ID %v specified in query", vpcID), nil)
			}
		}
		filterInput.VpcIDs = vpcIDs
	}

	// Get peerTenantId from query param
	peerTenantIDStrs := qParams["peerTenantId"]
	if len(peerTenantIDStrs) > 0 {
		cotel.SetAttribute(handlerSpan, attribute.StringSlice("peerTenantId", peerTenantIDStrs))
		peerTenantIDs := make([]uuid.UUID, 0, len(peerTenantIDStrs))
		for _, peerTenantIDStr := range peerTenantIDStrs {
			peerTenantID, err := uuid.Parse(peerTenantIDStr)
			if err != nil {
				logger.Warn().Msg(fmt.Sprintf("invalid value in peerTenantId query: %v", peerTenantIDStr))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid peer tenant ID %v in query", peerTenantIDStr), nil)
			}
			peerTenantIDs = append(peerTenantIDs, peerTenantID)
		}
		tnDAO := cdbm.NewTenantDAO(gavph.dbSession)
		peerTenants, _, err := tnDAO.GetAll(
			ctx,
			nil,
			cdbm.TenantFilterInput{TenantIDs: peerTenantIDs},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Tenants from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve tenants specified in query", nil)
		}
		peerTenantIDsMap := make(map[uuid.UUID]struct{}, len(peerTenants))
		for _, peerTenant := range peerTenants {
			peerTenantIDsMap[peerTenant.ID] = struct{}{}
		}
		for _, peerTenantID := range peerTenantIDs {
			if _, ok := peerTenantIDsMap[peerTenantID]; !ok {
				logger.Warn().Msg(fmt.Sprintf("could not find tenant with ID %v specified in query", peerTenantID))
				return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find tenant with ID %v specified in query", peerTenantID), nil)
			}
		}
		filterInput.PeerTenantIDs = peerTenantIDs
	}

	// Validate pagination request
	pageRequest := pagination.PageRequest{}
	err := c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate pagination attributes and set defaults.
	err = pageRequest.Validate(cdbm.VpcPeeringOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	if isMultiTenant != nil {
		filterInput.IsMultiTenant = isMultiTenant
	}

	if infrastructureProvider != nil {
		filterInput.InfrastructureProviderIDs = []uuid.UUID{infrastructureProvider.ID}
	}

	if tenant != nil {
		filterInput.TenantIDs = []uuid.UUID{tenant.ID}
	}

	// Get VPC Peerings from DB
	vpcPeeringDAO := cdbm.NewVpcPeeringDAO(gavph.dbSession)
	vpcPeeringPageInput := cdbp.PageInput{
		Limit:   pageRequest.Limit,
		Offset:  pageRequest.Offset,
		OrderBy: pageRequest.OrderBy,
	}

	// If VPC ID or peer tenant ID is specified, include the VPC and tenant relations
	if len(vpcIDStrs) > 0 || len(peerTenantIDStrs) > 0 {
		for _, relation := range []string{cdbm.Vpc1RelationName, cdbm.Vpc2RelationName, cdbm.TenantRelationName} {
			if !slices.Contains(qIncludeRelations, relation) {
				qIncludeRelations = append(qIncludeRelations, relation)
			}
		}
	}
	vpcPeerings, total, err := vpcPeeringDAO.GetAll(ctx, nil, filterInput, vpcPeeringPageInput, qIncludeRelations)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving VPC Peerings from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPC Peerings, DB error", nil)
	}

	dbMappedPeeringTenants := make(map[uuid.UUID]*cdbm.Tenant)
	vpcPeeringTenantIDs := []uuid.UUID{}
	for _, vpcPeering := range vpcPeerings {
		if vpcPeering.Vpc1 != nil {
			vpcPeeringTenantIDs = append(vpcPeeringTenantIDs, vpcPeering.Vpc1.TenantID)
		}
		if vpcPeering.Vpc2 != nil {
			vpcPeeringTenantIDs = append(vpcPeeringTenantIDs, vpcPeering.Vpc2.TenantID)
		}
	}
	if len(vpcPeeringTenantIDs) > 0 {
		tenantDAO := cdbm.NewTenantDAO(gavph.dbSession)
		tenants, _, err := tenantDAO.GetAll(ctx, nil, cdbm.TenantFilterInput{TenantIDs: vpcPeeringTenantIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Tenants from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenants, DB error", nil)
		}
		for _, tenant := range tenants {
			dbMappedPeeringTenants[tenant.ID] = &tenant
		}
	}

	// Build API response
	apiVpcPeerings := make([]model.APIVpcPeering, len(vpcPeerings))
	for i, vpcPeering := range vpcPeerings {
		apiVpcPeerings[i] = model.NewAPIVpcPeering(vpcPeering, dbMappedPeeringTenants)
	}

	// Create pagination response header
	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}

	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiVpcPeerings)
}

// ~~~~~ Get VPC Peering Handler ~~~~~ //

// GetVpcPeeringHandler is the API Handler for getting a VPC Peering
type GetVpcPeeringHandler struct {
	dbSession *cdb.Session
	tc        tclient.Client
	cfg       *config.Config
}

// NewGetVpcPeeringHandler initializes and returns a new handler to retrieve VPC Peering
func NewGetVpcPeeringHandler(dbSession *cdb.Session, tc tclient.Client, cfg *config.Config) GetVpcPeeringHandler {
	return GetVpcPeeringHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Retrieve a VPC Peering
// @Description Retrieve a VPC Peering by ID for the current user
// @Tags vpcpeering
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of VPC Peering"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Vpc1', 'Vpc2', 'Site', 'Tenant'""
// @Success 200 {object} model.APIVpcPeering
// @Router /v2/org/{org}/nico/vpc-peering/{id} [get]
func (gvph GetVpcPeeringHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Get", "VpcPeering", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Ensure our user is a provider or tenant for the org
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, gvph.dbSession, org, dbUser, false, nil)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	peeringID := c.Param("id")
	logger = logger.With().Str("Peering ID", peeringID).Logger()
	cotel.SetAttribute(handlerSpan, attribute.String("peering_id", peeringID))

	// Parse and validate peering ID
	peeringUUID, err := uuid.Parse(peeringID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing VPC Peering ID in URL")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid VPC Peering ID in URL parameter", nil)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.VpcPeeringRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Get VPC Peering from DB by ID
	vpcPeeringDAO := cdbm.NewVpcPeeringDAO(gvph.dbSession)
	vpcPeering, err := vpcPeeringDAO.GetByID(ctx, nil, peeringUUID, qIncludeRelations)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find VPC Peering with ID: %s", peeringUUID.String()), nil)
		}
		logger.Error().Err(err).Msg("error retrieving VPC Peering from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPC Peering", nil)
	}

	// Validate if user is authorized to get the VPC Peering
	providerAuthorized := infrastructureProvider != nil && vpcPeering.InfrastructureProviderID != nil && *vpcPeering.InfrastructureProviderID == infrastructureProvider.ID
	tenantAuthorized := false
	if !providerAuthorized && tenant != nil {
		// Get two VPCs of the VPC Peering
		vpcDAO := cdbm.NewVpcDAO(gvph.dbSession)
		vpc1, err := vpcDAO.GetByID(ctx, nil, vpcPeering.Vpc1ID, []string{cdbm.TenantRelationName})
		if err != nil {
			if err == cdb.ErrDoesNotExist {
				return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find VPC with ID: %s", vpcPeering.Vpc1ID.String()), nil)
			}
			logger.Error().Err(err).Msg("error retrieving VPC 1 of VPC Peering from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to retrieve VPC 1 with ID: %s, DB error", vpcPeering.Vpc1ID.String()), nil)
		}

		vpc2, err := vpcDAO.GetByID(ctx, nil, vpcPeering.Vpc2ID, []string{cdbm.TenantRelationName})
		if err != nil {
			if err == cdb.ErrDoesNotExist {
				return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find VPC with ID: %s", vpcPeering.Vpc2ID.String()), nil)
			}
			logger.Error().Err(err).Msg("error retrieving VPC 2 of VPC Peering from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to retrieve VPC 2 with ID: %s, DB error", vpcPeering.Vpc2ID.String()), nil)
		}

		// Validate that Tenant owns one of the VPCs of the VPC Peering
		if vpc1.TenantID == tenant.ID || vpc2.TenantID == tenant.ID {
			tenantAuthorized = true
		} else {
			if vpc1.TenantID != tenant.ID {
				logger.Warn().Str("vpc_id", vpc1.ID.String()).Str("tenant_id", tenant.ID.String()).Msg("VPC 1 does not belong to Tenant associated with current org")
			}
			if vpc2.TenantID != tenant.ID {
				logger.Warn().Str("vpc_id", vpc2.ID.String()).Str("tenant_id", tenant.ID.String()).Msg("VPC 2 does not belong to Tenant associated with current org")
			}
		}
	}

	if !providerAuthorized && !tenantAuthorized {
		logger.Warn().Msg("User does not have access to the VPC Peering")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have access to the VPC Peering", nil)
	}

	// Map tenant to the VPCs
	dbMappedPeeringTenants := make(map[uuid.UUID]*cdbm.Tenant)
	tenantIDs := []uuid.UUID{}

	if vpcPeering.Vpc1 != nil {
		tenantIDs = append(tenantIDs, vpcPeering.Vpc1.TenantID)
	}
	if vpcPeering.Vpc2 != nil {
		tenantIDs = append(tenantIDs, vpcPeering.Vpc2.TenantID)
	}

	// Fetch Tenants from DB if VPCs have tenants
	if len(tenantIDs) > 0 {
		tenantDAO := cdbm.NewTenantDAO(gvph.dbSession)
		tenants, _, err := tenantDAO.GetAll(ctx, nil, cdbm.TenantFilterInput{TenantIDs: tenantIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Tenants from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenants, DB error", nil)
		}
		for _, tenant := range tenants {
			dbMappedPeeringTenants[tenant.ID] = &tenant
		}
	}

	// Convert to API model
	apiVpcPeering := model.NewAPIVpcPeering(*vpcPeering, dbMappedPeeringTenants)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiVpcPeering)
}

// ~~~~~ Delete VPC Peering Handler ~~~~~ //

// DeleteVpcPeeringHandler is the API Handler for deleting a VPC Peering
type DeleteVpcPeeringHandler struct {
	dbSession *cdb.Session
	tc        tclient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteVpcPeeringHandler initializes and returns a new handler for deleting VPC Peering
func NewDeleteVpcPeeringHandler(dbSession *cdb.Session, tc tclient.Client, sc *sc.ClientPool, cfg *config.Config) DeleteVpcPeeringHandler {
	return DeleteVpcPeeringHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       sc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Delete a VPC Peering
// @Description Delete a VPC Peering by ID.
// @Tags vpcpeering
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of VPC Peering"
// @Success 204 "No Content"
// @Router /v2/org/{org}/nico/vpc-peering/{id} [delete]
func (dvph DeleteVpcPeeringHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Delete", "VpcPeering", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Ensure our user is a provider or tenant for the org
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, dvph.dbSession, org, dbUser, false, nil)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Get VPC Peering ID from URL param
	vpcPeeringID, err := uuid.Parse(c.Param("id"))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid VPC Peering ID", nil)
	}
	logger = logger.With().Str("VPC Peering ID", vpcPeeringID.String()).Logger()

	cotel.SetAttribute(handlerSpan, attribute.String("vpc_peering_id", vpcPeeringID.String()))

	// Get VPC Peering from DB by ID
	vpcPeeringDAO := cdbm.NewVpcPeeringDAO(dvph.dbSession)
	vpcPeering, err := vpcPeeringDAO.GetByID(ctx, nil, vpcPeeringID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find VPC Peering with ID: %s", vpcPeeringID.String()), nil)
		}
		logger.Error().Err(err).Msg("error retrieving VPC Peering from DB by ID")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPC Peering with specified ID", nil)
	}

	// Get two VPCs of the VPC Peering
	vpcDAO := cdbm.NewVpcDAO(dvph.dbSession)
	vpc1, err := vpcDAO.GetByID(ctx, nil, vpcPeering.Vpc1ID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find VPC with ID: %s", vpcPeering.Vpc1ID.String()), nil)
		}
		logger.Error().Err(err).Msg("error retrieving VPC 1 of VPC Peering from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to retrieve VPC 1 with ID: %s, DB error", vpcPeering.Vpc1ID.String()), nil)
	}
	vpc2, err := vpcDAO.GetByID(ctx, nil, vpcPeering.Vpc2ID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find VPC with ID: %s", vpcPeering.Vpc2ID.String()), nil)
		}
		logger.Error().Err(err).Msg("error retrieving VPC 2 of VPC Peering from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to retrieve VPC 2 with ID: %s, DB error", vpcPeering.Vpc2ID.String()), nil)
	}

	isMultiTenant := vpc1.TenantID != vpc2.TenantID

	// Validate if user is authorized to delete the VPC Peering
	providerAuthorized := false
	if infrastructureProvider != nil {
		// Provider Admin can delete peerings in sites provided by this org.
		// The deletion operation is not gated on TenantAccount/TenantSite checks to avoid
		// blocking cleanup.
		site, err := common.GetSiteFromIDString(ctx, nil, vpcPeering.SiteID.String(), dvph.dbSession)
		if err != nil {
			if errors.Is(err, cdb.ErrDoesNotExist) {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "VPC Peering site not found", nil)
			}
			logger.Error().Err(err).Msg("error retrieving site for VPC Peering")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPC Peering", nil)
		}

		if site.InfrastructureProviderID == infrastructureProvider.ID && isMultiTenant {
			logger.Info().Msg("Provider is authorized to delete VPC Peering in provided site")
			providerAuthorized = true
		}
	}

	tenantAuthorized := false
	if tenant != nil && !providerAuthorized {
		// Tenant-only users can only delete single-tenant peerings where both VPCs belong to them.
		if vpc1.TenantID == tenant.ID && vpc2.TenantID == tenant.ID {
			logger.Info().Msg("Tenant is authorized to delete single-tenant VPC Peering")
			tenantAuthorized = true
		} else {
			if vpc1.TenantID != tenant.ID {
				logger.Warn().Str("vpc_id", vpc1.ID.String()).Str("tenant_id", tenant.ID.String()).Msg("VPC 1 does not belong to Tenant associated with current org")
			}
			if vpc2.TenantID != tenant.ID {
				logger.Warn().Str("vpc_id", vpc2.ID.String()).Str("tenant_id", tenant.ID.String()).Msg("VPC 2 does not belong to Tenant associated with current org")
			}
		}
	}

	if !providerAuthorized && !tenantAuthorized {
		logger.Warn().Msg("User does not have access to delete the VPC Peering")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have access to delete the VPC Peering", nil)
	}

	// timeoutResp lets the closure signal a post-rollback handler — the
	// TerminateWorkflow call has to run after the closure returns so that
	// the DB tx unwinds before we make the second remote call. nil means
	// no timeout occurred and the normal flow continues.
	var timeoutResp func() error
	err = cdb.WithTx(ctx, dvph.dbSession, func(tx *cdb.Tx) error {
		// Update status to Deleting first
		derr := vpcPeeringDAO.UpdateStatusByID(ctx, tx, vpcPeering.ID, cdbm.VpcPeeringStatusDeleting)
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating VPC Peering status to Deleting")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update VPC Peering status to Deleting", nil)
		}

		// Create the VPC Peering deletion request
		deleteVpcPeeringRequest := vpcPeering.ToDeletionRequestProto()

		// Get the site temporal client
		stc, derr := dvph.scp.GetClientByID(vpcPeering.SiteID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Temporal client for Site", nil)
		}

		// Setup workflow options
		workflowOptions := tclient.StartWorkflowOptions{
			ID:                       "vpcpeering-delete-" + vpcPeering.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		logger.Info().Msg("triggering VPC Peering delete workflow")

		// Add context deadline
		workflowCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger site workflow to delete VPC Peering
		we, derr := stc.ExecuteWorkflow(workflowCtx, workflowOptions, "DeleteVpcPeering", deleteVpcPeeringRequest)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to start VPC Peering deletion workflow")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to start VPC Peering deletion workflow", nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("started VPC Peering deletion workflow")

		// Wait for workflow completion synchronously
		wferr := we.Get(workflowCtx, nil)
		if wferr != nil {
			var applicationErr *tp.ApplicationError
			if errors.As(wferr, &applicationErr) && slices.Contains(swe.UnimplementedOrDeniedErrTypes(), applicationErr.Type()) {
				logger.Error().Msg("feature not yet implemented on target Site")
				return cutil.NewAPIError(http.StatusNotImplemented, fmt.Sprintf("Feature not yet implemented on target Site: %s", wferr), nil)
			}

			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || workflowCtx.Err() != nil {
				logger.Error().Err(wferr).Msg("failed to delete VPC Peering, timeout occurred executing workflow on Site.")
				timeoutCause := wferr
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, timeoutCause, "VpcPeering", "DeleteVpcPeering")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "VPC Peering delete workflow timed out", nil)
			}

			logger.Error().Err(wferr).Msg("failed to synchronously execute Temporal workflow to delete VPC Peering")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to execute sync workflow to delete VPC Peering on Site: %s", wferr), nil)
		}

		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to delete VPC Peering, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// Best effort post-commit cleanup: remove VPC Peering from DB.
	// This is intentionally outside of the transaction so delete does not fail if this cleanup fails.
	derr := vpcPeeringDAO.Delete(ctx, nil, vpcPeering.ID)
	if derr != nil {
		logger.Warn().Err(derr).Msg("best-effort delete of VPC Peering from DB failed after workflow completion")
	}

	logger.Info().Msg("finishing API handler")

	return c.NoContent(http.StatusNoContent)
}
