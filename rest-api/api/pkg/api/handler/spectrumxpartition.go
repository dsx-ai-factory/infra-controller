// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"

	"go.opentelemetry.io/otel/attribute"

	goset "github.com/deckarep/golang-set/v2"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// NICo Core (forge.Forge) SpectrumX Partition methods proxied by these handlers.
const (
	createSpxPartitionMethod = "/forge.Forge/CreateSpxPartition"
	deleteSpxPartitionMethod = "/forge.Forge/DeleteSpxPartition"
)

// authorizeTenantForSpectrumXPartition runs the gates every SpectrumX Partition handler
// shares: the caller belongs to the org, holds Tenant Admin, and the org resolves to a
// Tenant. It returns that Tenant.
func authorizeTenantForSpectrumXPartition(ctx context.Context, logger zerolog.Logger, dbSession *cdb.Session, org string, dbUser *cdbm.User) (*cdbm.Tenant, *cutil.APIError) {
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return nil, cutil.NewAPIError(http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Only Tenant Admins may manage SpectrumX Partitions.
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return nil, cutil.NewAPIError(http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	tenant, err := common.GetTenantForOrg(ctx, nil, dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			logger.Warn().Err(err).Msg("Org does not have a Tenant associated")
			return nil, cutil.NewAPIError(http.StatusBadRequest, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("unable to retrieve Tenant for org")
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Tenant for org", nil)
	}

	return tenant, nil
}

// ~~~~~ Create Handler ~~~~~ //

// CreateSpectrumXPartitionHandler is the API Handler for creating a new SpectrumXPartition
type CreateSpectrumXPartitionHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewCreateSpectrumXPartitionHandler returns a handler for creating a SpectrumXPartition
func NewCreateSpectrumXPartitionHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) CreateSpectrumXPartitionHandler {
	return CreateSpectrumXPartitionHandler{
		dbSession:  dbSession,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Create a SpectrumXPartition
// @Description Create a SpectrumXPartition
// @Tags SpectrumXPartition
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APISpectrumXPartitionCreateRequest true "SpectrumXPartition creation request"
// @Success 201 {object} model.APISpectrumXPartition
// @Router /v2/org/{org}/nico/spectrumx-partition [post]
func (csxph CreateSpectrumXPartitionHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("SpectrumXPartition", "Create", c, csxph.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	orgTenant, apiErr := authorizeTenantForSpectrumXPartition(ctx, logger, csxph.dbSession, org, dbUser)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	// Bind request data to API model
	apiRequest := model.APISpectrumXPartitionCreateRequest{}
	err := c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating SpectrumX Partition creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating SpectrumX Partition request creation data", verr)
	}

	// Validate and verify the Site is ready
	site, serr := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, csxph.dbSession)
	if serr != nil {
		if serr == common.ErrInvalidID {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create SpectrumX Partition, Invalid Site ID: %s", apiRequest.SiteID), nil)
		}
		if serr == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Failed to create SpectrumX Partition, Could not find Site with ID: %s ", apiRequest.SiteID), nil)
		}
		logger.Warn().Err(serr).Str("Site ID", apiRequest.SiteID).Msg("error retrieving Site from DB by ID")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create SpectrumX Partition, Could not find Site with ID: %s, DB error", apiRequest.SiteID), nil)
	}

	if site.Status != cdbm.SiteStatusRegistered {
		logger.Warn().Str("Site ID", site.ID.String()).Msg("Site is not in Registered state")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create SpectrumX Partition, Site: %s specified in request is not in Registered state", site.ID.String()), nil)
	}

	// Determine if the Tenant has access to the requested Site
	tsDAO := cdbm.NewTenantSiteDAO(csxph.dbSession)
	_, err = tsDAO.GetByTenantIDAndSiteID(ctx, nil, orgTenant.ID, site.ID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant is not associated with Site specified in query", nil)
		}
		logger.Warn().Err(err).Msg("error retrieving Tenant Site association from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to determine if Tenant has access to Site specified in query, DB error", nil)
	}

	// Ensure that the Tenant has an Allocation with the specified Site
	aDAO := cdbm.NewAllocationDAO(csxph.dbSession)
	aCount, serr := aDAO.GetCount(ctx, nil, cdbm.AllocationFilterInput{TenantIDs: []uuid.UUID{orgTenant.ID}, SiteIDs: []uuid.UUID{site.ID}})
	if serr != nil {
		logger.Error().Err(serr).Msg("error retrieving Allocations count from DB for Tenant and Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site Allocations count for Tenant", nil)
	}
	if aCount == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have any Allocations with Site specified in request data", nil)
	}

	// A Tenant cannot have two SpectrumX Partitions with the same name on one Site.
	// TODO consider doing this with an advisory lock for correctness
	sxpDAO := cdbm.NewSpectrumXPartitionDAO(csxph.dbSession)
	existing, tot, err := sxpDAO.GetAll(
		ctx,
		nil,
		cdbm.SpectrumXPartitionFilterInput{
			Names:     []string{apiRequest.Name},
			SiteIDs:   []uuid.UUID{site.ID},
			TenantIDs: []uuid.UUID{orgTenant.ID},
		},
		paginator.PageInput{},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("db error checking for name uniqueness of tenant SpectrumX Partition")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create SpectrumX Partition due to DB error", nil)
	}
	if tot > 0 {
		logger.Warn().Str("tenantId", orgTenant.ID.String()).Str("name", apiRequest.Name).Msg("SpectrumX Partition with same name already exists for Tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Another SpectrumX Partition with specified name already exists for Tenant", validation.Errors{
			"id": errors.New(existing[0].ID.String()),
		})
	}

	stc, derr := csxph.scp.GetClientByID(site.ID)
	if derr != nil {
		logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	sdDAO := cdbm.NewStatusDetailDAO(csxph.dbSession)

	var createdSXP *cdbm.SpectrumXPartition
	var ssd *cdbm.StatusDetail

	err = cdb.WithTx(ctx, csxph.dbSession, func(tx *cdb.Tx) error {
		sxp, derr := sxpDAO.Create(
			ctx,
			tx,
			cdbm.SpectrumXPartitionCreateInput{
				Name:        apiRequest.Name,
				Description: apiRequest.Description,
				TenantOrg:   org,
				SiteID:      site.ID,
				TenantID:    orgTenant.ID,
				VNI:         apiRequest.VNI,
				Labels:      apiRequest.Labels,
				Status:      cdbm.SpectrumXPartitionStatusPending,
				CreatedBy:   dbUser.ID,
			},
		)
		if derr != nil {
			logger.Error().Err(derr).Msg("unable to create SpectrumX Partition record in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed creating SpectrumX Partition record", nil)
		}

		newSSD, derr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{
			EntityID: sxp.ID.String(),
			Status:   string(cdbm.SpectrumXPartitionStatusPending),
			Message:  cutil.GetPtr("received SpectrumX Partition creation request, pending"),
		})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for SpectrumX Partition", nil)
		}
		if newSSD == nil {
			logger.Error().Msg("Status Detail DB entry not returned from Create")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to get new Status Detail for SpectrumX Partition", nil)
		}
		ssd = newSSD

		logger.Info().Str("SpectrumX Partition ID", sxp.ID.String()).Msg("creating SpectrumX Partition via Core proxy")

		// The Site allocates the VNI when the request omits one, so capture the
		// response rather than discarding it.
		coreResp := &corev1.SpxPartition{}
		if proxyErr := common.ExecuteCoreGRPC(ctx, stc, createSpxPartitionMethod, apiRequest.ToProto(sxp), coreResp, ""); proxyErr != nil {
			logAPIError(logger, proxyErr, "failed to create SpectrumX Partition on Site")
			return cutil.NewAPIError(proxyErr.Code, proxyErr.Message, nil)
		}

		// Record the Site-allocated VNI so the create response carries it instead of
		// making the caller wait for the next inventory cycle.
		if coreResp.Vni != 0 {
			vni := int(coreResp.Vni)
			sxp, derr = sxpDAO.Update(ctx, tx, cdbm.SpectrumXPartitionUpdateInput{
				SpectrumXPartitionID: sxp.ID,
				VNI:                  &vni,
			})
			if derr != nil {
				logger.Error().Err(derr).Msg("error recording Site allocated VNI for SpectrumX Partition")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to record VNI for SpectrumX Partition", nil)
			}
		}

		createdSXP = sxp
		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to create SpectrumX Partition, DB transaction error")
	}

	apiSXP := &model.APISpectrumXPartition{}
	apiSXP.FromDB(createdSXP, []cdbm.StatusDetail{*ssd})

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusCreated, apiSXP)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllSpectrumXPartitionHandler is the API Handler for retrieving all SpectrumXPartitions
type GetAllSpectrumXPartitionHandler struct {
	dbSession  *cdb.Session
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetAllSpectrumXPartitionHandler returns a handler for retrieving all SpectrumXPartitions
func NewGetAllSpectrumXPartitionHandler(dbSession *cdb.Session, cfg *config.Config) GetAllSpectrumXPartitionHandler {
	return GetAllSpectrumXPartitionHandler{
		dbSession:  dbSession,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get all SpectrumXPartitions
// @Description Get all SpectrumXPartitions
// @Tags SpectrumXPartition
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string false "ID of Site"
// @Param status query string false "Filter by status e.g. 'Pending', 'Error'"
// @Param query query string false "Query input for full text search"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Site', 'Tenant'"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APISpectrumXPartition
// @Router /v2/org/{org}/nico/spectrumx-partition [get]
func (gasxph GetAllSpectrumXPartitionHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("SpectrumXPartition", "GetAll", c, gasxph.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	tenant, apiErr := authorizeTenantForSpectrumXPartition(ctx, logger, gasxph.dbSession, org, dbUser)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	pageRequest := pagination.PageRequest{}
	err := c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	err = pageRequest.Validate(cdbm.SpectrumXPartitionOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Get Site ID from query param
	tsDAO := cdbm.NewTenantSiteDAO(gasxph.dbSession)
	var siteIDs []uuid.UUID
	siteIDStr := c.QueryParam("siteId")
	if siteIDStr != "" {
		site, err := common.GetSiteFromIDString(ctx, nil, siteIDStr, gasxph.dbSession)
		if err != nil {
			logger.Warn().Err(err).Msg("error getting Site in request")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Site specified in query param, invalid ID or DB error", nil)
		}
		siteIDs = append(siteIDs, site.ID)

		_, err = tsDAO.GetByTenantIDAndSiteID(ctx, nil, tenant.ID, site.ID, nil)
		if err != nil {
			if err == cdb.ErrDoesNotExist {
				return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have access to this Site", nil)
			}
			logger.Error().Err(err).Msg("error retrieving TenantSite from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to determine Tenant access to Site, DB error", nil)
		}
	}

	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.SpectrumXPartitionRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	searchQuery := common.GetSearchQuery(c)
	if searchQuery != nil {
		gasxph.tracerSpan.SetAttribute(handlerSpan, attribute.String("query", *searchQuery), logger)
	}

	var statuses []string
	statusQuery := c.QueryParam("status")
	if statusQuery != "" {
		gasxph.tracerSpan.SetAttribute(handlerSpan, attribute.String("status", statusQuery), logger)
		if !cdbm.SpectrumXPartitionStatusMap[cdbm.SpectrumXPartitionStatus(statusQuery)] {
			logger.Warn().Str("status", statusQuery).Msg("invalid value in status query")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Status value in query", nil)
		}
		statuses = append(statuses, statusQuery)
	}

	sxpDAO := cdbm.NewSpectrumXPartitionDAO(gasxph.dbSession)
	sxps, total, err := sxpDAO.GetAll(
		ctx,
		nil,
		cdbm.SpectrumXPartitionFilterInput{
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
		logger.Error().Err(err).Msg("error getting SpectrumX Partitions from db")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SpectrumX Partitions, DB error", nil)
	}

	sdDAO := cdbm.NewStatusDetailDAO(gasxph.dbSession)
	sdEntityIDs := []string{}
	for _, sxp := range sxps {
		sdEntityIDs = append(sdEntityIDs, sxp.ID.String())
	}

	ssds, serr := sdDAO.GetRecentByEntityIDs(ctx, nil, sdEntityIDs, common.RECENT_STATUS_DETAIL_COUNT)
	if serr != nil {
		logger.Warn().Err(serr).Msg("error retrieving Status Details for SpectrumX Partitions from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate status history for SpectrumX Partitions", nil)
	}

	ssdMap := map[string][]cdbm.StatusDetail{}
	for _, ssd := range ssds {
		cssd := ssd
		ssdMap[ssd.EntityID] = append(ssdMap[ssd.EntityID], cssd)
	}

	apiSXPs := []*model.APISpectrumXPartition{}
	for _, sxp := range sxps {
		curSXP := sxp
		apiSXP := &model.APISpectrumXPartition{}
		apiSXP.FromDB(&curSXP, ssdMap[sxp.ID.String()])
		apiSXPs = append(apiSXPs, apiSXP)
	}

	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}

	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiSXPs)
}

// ~~~~~ Get Handler ~~~~~ //

// GetSpectrumXPartitionHandler is the API Handler for retrieving a SpectrumXPartition
type GetSpectrumXPartitionHandler struct {
	dbSession  *cdb.Session
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetSpectrumXPartitionHandler returns a handler for retrieving a SpectrumXPartition
func NewGetSpectrumXPartitionHandler(dbSession *cdb.Session, cfg *config.Config) GetSpectrumXPartitionHandler {
	return GetSpectrumXPartitionHandler{
		dbSession:  dbSession,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get a SpectrumXPartition
// @Description Get a SpectrumXPartition
// @Tags SpectrumXPartition
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param spectrumXPartitionId path string true "ID of SpectrumXPartition"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Site', 'Tenant'"
// @Success 200 {object} model.APISpectrumXPartition
// @Router /v2/org/{org}/nico/spectrumx-partition/{spectrumXPartitionId} [get]
func (gsxph GetSpectrumXPartitionHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("SpectrumXPartition", "Get", c, gsxph.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	orgTenant, apiErr := authorizeTenantForSpectrumXPartition(ctx, logger, gsxph.dbSession, org, dbUser)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	sxpID, err := uuid.Parse(c.Param("id"))
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid SpectrumX Partition ID in URL", nil)
	}

	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.SpectrumXPartitionRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	sxpDAO := cdbm.NewSpectrumXPartitionDAO(gsxph.dbSession)
	sxp, err := sxpDAO.Get(ctx, nil, sxpID, qIncludeRelations)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find SpectrumX Partition with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving SpectrumX Partition DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SpectrumX Partition, DB error", nil)
	}

	if sxp.TenantID != orgTenant.ID {
		logger.Warn().Msg("Tenant in SpectrumX Partition does not belong to Tenant in org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant for SpectrumX Partition in request does not match Tenant in org", nil)
	}

	sdDAO := cdbm.NewStatusDetailDAO(gsxph.dbSession)
	ssds, serr := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{sxp.ID.String()}, common.RECENT_STATUS_DETAIL_COUNT)
	if serr != nil {
		logger.Warn().Err(serr).Msg("error retrieving Status Details for SpectrumX Partition from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate status history for SpectrumX Partition", nil)
	}

	apiSXP := &model.APISpectrumXPartition{}
	apiSXP.FromDB(sxp, ssds)

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiSXP)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteSpectrumXPartitionHandler is the API Handler for deleting a SpectrumXPartition
type DeleteSpectrumXPartitionHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewDeleteSpectrumXPartitionHandler returns a handler for deleting a SpectrumXPartition
func NewDeleteSpectrumXPartitionHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) DeleteSpectrumXPartitionHandler {
	return DeleteSpectrumXPartitionHandler{
		dbSession:  dbSession,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Delete a SpectrumXPartition
// @Description Delete a SpectrumXPartition
// @Tags SpectrumXPartition
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param spectrumXPartitionId path string true "ID of SpectrumXPartition"
// @Success 202 {object} model.APIMessageResponse
// @Router /v2/org/{org}/nico/spectrumx-partition/{spectrumXPartitionId} [delete]
func (dsxph DeleteSpectrumXPartitionHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("SpectrumXPartition", "Delete", c, dsxph.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	orgTenant, apiErr := authorizeTenantForSpectrumXPartition(ctx, logger, dsxph.dbSession, org, dbUser)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	sxpID, err := uuid.Parse(c.Param("id"))
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid SpectrumX Partition ID in URL", nil)
	}

	sxpDAO := cdbm.NewSpectrumXPartitionDAO(dsxph.dbSession)
	sxp, err := sxpDAO.Get(ctx, nil, sxpID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not retrieve SpectrumX Partition to delete", nil)
		}
		logger.Error().Err(err).Msg("error retrieving SpectrumX Partition DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Could not retrieve SpectrumX Partition to delete", nil)
	}

	if sxp.TenantID != orgTenant.ID {
		logger.Warn().Msg("Tenant in SpectrumX Partition does not belong to Tenant in org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant for SpectrumX Partition in request does not match Tenant in org", nil)
	}

	// Block deletion while Instances referenced by SpectrumX Attachments still exist in the DB
	sxaDAO := cdbm.NewSpectrumXAttachmentDAO(dsxph.dbSession)
	attachments, _, err := sxaDAO.GetAll(ctx, nil, cdbm.SpectrumXAttachmentFilterInput{
		SpectrumXPartitionIDs: []uuid.UUID{sxpID},
	}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving SpectrumX Attachments from DB for SpectrumX Partition")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SpectrumX Attachments for SpectrumX Partition", nil)
	}
	instanceIDSet := goset.NewSet[uuid.UUID]()
	for _, sxa := range attachments {
		instanceIDSet.Add(sxa.InstanceID)
	}
	if instanceIDSet.Cardinality() > 0 {
		instanceDAO := cdbm.NewInstanceDAO(dsxph.dbSession)
		activeCount, err := instanceDAO.GetCount(ctx, nil, cdbm.InstanceFilterInput{
			InstanceIDs: instanceIDSet.ToSlice(),
		})
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving count of Instances from DB for SpectrumX Partition attachment check")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve count of Instances for SpectrumX Partition", nil)
		}
		if activeCount > 0 {
			logger.Warn().Int("active_instance_count", activeCount).Msg("SpectrumX Partition has active Instances associated via attachments")
			msg := fmt.Sprintf("%d active Instances are associated with this SpectrumX Partition, unable to delete", activeCount)
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, msg, nil)
		}
	}

	stc, derr := dsxph.scp.GetClientByID(sxp.SiteID)
	if derr != nil {
		logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	sdDAO := cdbm.NewStatusDetailDAO(dsxph.dbSession)

	err = cdb.WithTx(ctx, dsxph.dbSession, func(tx *cdb.Tx) error {
		// The row is left in Deleting rather than removed here; inventory removes it
		// once the Site stops reporting the Partition.
		deletingStatus := cdbm.SpectrumXPartitionStatusDeleting
		if _, derr := sxpDAO.Update(ctx, tx, cdbm.SpectrumXPartitionUpdateInput{
			SpectrumXPartitionID: sxp.ID,
			Status:               &deletingStatus,
		}); derr != nil {
			logger.Error().Err(derr).Msg("error updating SpectrumX Partition in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete SpectrumX Partition, DB error", nil)
		}

		if _, derr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{
			EntityID: sxp.ID.String(),
			Status:   string(deletingStatus),
			Message:  cutil.GetPtr("Received request for deletion, pending processing"),
		}); derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for SpectrumX Partition deletion", nil)
		}

		logger.Info().Str("SpectrumX Partition ID", sxp.ID.String()).Msg("deleting SpectrumX Partition via Core proxy")

		if proxyErr := common.ExecuteCoreGRPC(ctx, stc, deleteSpxPartitionMethod, sxp.ToDeletionRequestProto(), nil, ""); proxyErr != nil {
			// A Partition the Site no longer knows about is already in the state the
			// caller asked for, so let the deletion proceed.
			if proxyErr.Code == http.StatusNotFound {
				logger.Warn().Msg("SpectrumX Partition not found on Site, treating as already deleted")
				return nil
			}
			logAPIError(logger, proxyErr, "failed to delete SpectrumX Partition on Site")
			return cutil.NewAPIError(proxyErr.Code, proxyErr.Message, nil)
		}

		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to delete SpectrumX Partition, DB transaction error")
	}

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusAccepted, model.NewAPIDeletionAcceptedResponse())
}
