// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// CreateDomainHandler creates a tenant-owned DNS Domain on one Site.
type CreateDomainHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// NewCreateDomainHandler returns a Domain creation handler.
func NewCreateDomainHandler(dbSession *cdb.Session, scp *sc.ClientPool) CreateDomainHandler {
	return CreateDomainHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle creates a tenant-owned DNS Domain.
func (cdh CreateDomainHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Domain", "Create", c, cdh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	apiRequest := model.APIDomainCreateRequest{}
	err := c.Bind(&apiRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	err = apiRequest.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Domain creation request data", err)
	}

	tenant, apiErr := common.IsTenant(ctx, logger, cdh.dbSession, org, dbUser, nil)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}
	site, apiErr := getDomainSiteForTenant(ctx, logger, cdh.dbSession, tenant, apiRequest.SiteID, true)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	stc, err := cdh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	coreDomain := &corev1.Domain{}
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_CreateDomain_FullMethodName, apiRequest.ToProto(), coreDomain, site.ID.String())
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to create Domain via Core proxy")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	controllerDomainID, err := uuid.Parse(coreDomain.GetId().GetValue())
	if err != nil || controllerDomainID == uuid.Nil {
		logger.Error().Err(err).Str("controllerDomainID", coreDomain.GetId().GetValue()).Msg("Core returned an invalid Domain ID")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Core returned an unexpected Domain create response", nil)
	}

	domainDAO := cdbm.NewDomainDAO(cdh.dbSession)
	domain, err := cdb.WithTxResult(ctx, cdh.dbSession, func(tx *cdb.Tx) (*cdbm.Domain, error) {
		return domainDAO.Create(ctx, tx, cdbm.DomainCreateInput{
			Hostname:           apiRequest.Name,
			Org:                org,
			TenantID:           &tenant.ID,
			SiteID:             &site.ID,
			ControllerDomainID: &controllerDomainID,
			Status:             cdbm.DomainStatusReady,
			CreatedBy:          dbUser.ID,
		})
	})
	if err != nil {
		logger.Error().Err(err).Str("controllerDomainID", controllerDomainID.String()).Msg("Domain created in Core but failed to update REST DB")
		cleanupCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), cutil.WorkflowContextTimeout)
		cleanupErr := common.ExecuteCoreGRPC(cleanupCtx, stc, corev1.Forge_DeleteDomain_FullMethodName, &corev1.DomainDeletionRequest{
			Id: &corev1.DomainId{Value: controllerDomainID.String()},
		}, nil, site.ID.String())
		cancel()
		if cleanupErr != nil {
			logAPIError(logger, cleanupErr, "failed to compensate Core Domain after REST DB failure")
		}
		return common.HandleTxError(c, logger, err, "Failed to create Domain, DB transaction error")
	}

	return c.JSON(http.StatusCreated, model.NewAPIDomain(domain))
}

// GetAllDomainHandler lists tenant-owned DNS Domains.
type GetAllDomainHandler struct {
	dbSession  *cdb.Session
	tracerSpan *cutil.TracerSpan
}

// NewGetAllDomainHandler returns a Domain list handler.
func NewGetAllDomainHandler(dbSession *cdb.Session) GetAllDomainHandler {
	return GetAllDomainHandler{
		dbSession:  dbSession,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle lists tenant-owned DNS Domains, optionally limited to one authorized Site.
func (gadh GetAllDomainHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Domain", "GetAll", c, gadh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	apiRequest := model.APIDomainGetAllRequest{}
	pageRequest := pagination.PageRequest{}
	err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest, pageRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = c.Bind(&apiRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse Domain list request data", nil)
	}
	err = apiRequest.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Domain list request data", err)
	}
	err = c.Bind(&pageRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}
	err = pageRequest.Validate(cdbm.DomainOrderByFields)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	tenant, apiErr := common.IsTenant(ctx, logger, gadh.dbSession, org, dbUser, nil)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}
	if apiRequest.TenantID != "" && apiRequest.TenantID != tenant.ID.String() {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant ID specified in query param does not belong to org", nil)
	}

	filter := cdbm.DomainFilterInput{TenantIDs: []uuid.UUID{tenant.ID}}
	if apiRequest.SiteID != "" {
		site, siteAPIError := getDomainSiteForTenant(ctx, logger, gadh.dbSession, tenant, apiRequest.SiteID, false)
		if siteAPIError != nil {
			return cutil.NewAPIErrorResponse(c, siteAPIError.Code, siteAPIError.Message, siteAPIError.Data)
		}
		filter.SiteIDs = []uuid.UUID{site.ID}
	}

	domains, total, err := cdbm.NewDomainDAO(gadh.dbSession).GetAll(ctx, nil, filter, pageRequest.ConvertToDB(), nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Domains from REST DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Domains, DB error", nil)
	}

	response := make([]*model.APIDomain, 0, len(domains))
	for i := range domains {
		response = append(response, model.NewAPIDomain(&domains[i]))
	}

	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("failed to marshal Domain pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	return c.JSON(http.StatusOK, response)
}

// GetDomainHandler retrieves one tenant-owned DNS Domain by its REST-local ID.
type GetDomainHandler struct {
	dbSession  *cdb.Session
	tracerSpan *cutil.TracerSpan
}

// NewGetDomainHandler returns a single-Domain retrieval handler.
func NewGetDomainHandler(dbSession *cdb.Session) GetDomainHandler {
	return GetDomainHandler{
		dbSession:  dbSession,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle retrieves one tenant-owned DNS Domain.
func (gdh GetDomainHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Domain", "Get", c, gdh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	domainID, err := uuid.Parse(c.Param("domainId"))
	if err != nil || domainID == uuid.Nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Domain ID in URL", nil)
	}

	tenant, apiErr := common.IsTenant(ctx, logger, gdh.dbSession, org, dbUser, nil)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}
	domain, err := getOwnedDomain(ctx, gdh.dbSession, domainID, tenant.ID)
	if errors.Is(err, cdb.ErrDoesNotExist) {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Domain with the specified ID", nil)
	}
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Domain from REST DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Domain, DB error", nil)
	}

	return c.JSON(http.StatusOK, model.NewAPIDomain(domain))
}

// UpdateDomainHandler renames a tenant-owned DNS Domain in Core and its REST projection.
type UpdateDomainHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// NewUpdateDomainHandler returns a Domain update handler.
func NewUpdateDomainHandler(dbSession *cdb.Session, scp *sc.ClientPool) UpdateDomainHandler {
	return UpdateDomainHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle renames a tenant-owned DNS Domain.
func (udh UpdateDomainHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Domain", "Update", c, udh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	domainID, err := uuid.Parse(c.Param("domainId"))
	if err != nil || domainID == uuid.Nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Domain ID in URL", nil)
	}

	apiRequest := model.APIDomainUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	err = apiRequest.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Domain update request data", err)
	}

	tenant, apiErr := common.IsTenant(ctx, logger, udh.dbSession, org, dbUser, nil)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}
	domain, err := getOwnedDomain(ctx, udh.dbSession, domainID, tenant.ID)
	if errors.Is(err, cdb.ErrDoesNotExist) {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Domain with the specified ID", nil)
	}
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Domain from REST DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Domain, DB error", nil)
	}
	if domain.SiteID == nil || domain.ControllerDomainID == nil || *domain.SiteID == uuid.Nil || *domain.ControllerDomainID == uuid.Nil {
		logger.Error().Str("domainID", domain.ID.String()).Msg("owned Domain projection is missing Site or Core identity")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Domain projection is missing required Site or Core identity", nil)
	}

	site, apiErr := getDomainSiteForTenant(ctx, logger, udh.dbSession, tenant, domain.SiteID.String(), true)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}
	stc, err := udh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	apiRequest.ControllerDomainID = *domain.ControllerDomainID
	coreDomain := &corev1.Domain{}
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_UpdateDomain_FullMethodName, apiRequest.ToProto(), coreDomain, site.ID.String())
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to update Domain via Core proxy")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	if coreDomain.GetId().GetValue() != domain.ControllerDomainID.String() || coreDomain.GetName() != apiRequest.Name {
		logger.Error().
			Str("domainID", domain.ID.String()).
			Str("controllerDomainID", domain.ControllerDomainID.String()).
			Str("returnedControllerDomainID", coreDomain.GetId().GetValue()).
			Msg("Core returned an unexpected Domain update response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Core returned an unexpected Domain update response", nil)
	}

	domainDAO := cdbm.NewDomainDAO(udh.dbSession)
	updatedDomain, err := cdb.WithTxResult(ctx, udh.dbSession, func(tx *cdb.Tx) (*cdbm.Domain, error) {
		return domainDAO.Update(ctx, tx, cdbm.DomainUpdateInput{
			DomainID: domain.ID,
			Hostname: &apiRequest.Name,
		})
	})
	if err != nil {
		logger.Error().Err(err).Msg("Domain updated in Core but failed to update REST DB")
		return common.HandleTxError(c, logger, err, "Failed to update Domain, DB transaction error")
	}

	return c.JSON(http.StatusOK, model.NewAPIDomain(updatedDomain))
}

// DeleteDomainHandler deletes a tenant-owned DNS Domain from Core and its REST projection.
type DeleteDomainHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// NewDeleteDomainHandler returns a Domain deletion handler.
func NewDeleteDomainHandler(dbSession *cdb.Session, scp *sc.ClientPool) DeleteDomainHandler {
	return DeleteDomainHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle deletes a tenant-owned DNS Domain.
func (ddh DeleteDomainHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Domain", "Delete", c, ddh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	domainID, err := uuid.Parse(c.Param("domainId"))
	if err != nil || domainID == uuid.Nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Domain ID in URL", nil)
	}

	tenant, apiErr := common.IsTenant(ctx, logger, ddh.dbSession, org, dbUser, nil)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}
	domain, err := getOwnedDomain(ctx, ddh.dbSession, domainID, tenant.ID)
	if errors.Is(err, cdb.ErrDoesNotExist) {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Domain with the specified ID", nil)
	}
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Domain from REST DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Domain, DB error", nil)
	}
	if domain.SiteID == nil || domain.ControllerDomainID == nil || *domain.SiteID == uuid.Nil || *domain.ControllerDomainID == uuid.Nil {
		logger.Error().Str("domainID", domain.ID.String()).Msg("owned Domain projection is missing Site or Core identity")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Domain projection is missing required Site or Core identity", nil)
	}

	site, apiErr := getDomainSiteForTenant(ctx, logger, ddh.dbSession, tenant, domain.SiteID.String(), true)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	_, subnetCount, err := cdbm.NewSubnetDAO(ddh.dbSession).GetAll(ctx, nil, cdbm.SubnetFilterInput{
		DomainIDs: []uuid.UUID{domain.ID},
	}, cdbp.PageInput{Limit: cutil.GetPtr(0)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Subnet references for Domain")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to determine whether Domain is in use, DB error", nil)
	}
	if subnetCount > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusPreconditionFailed, "Cannot delete Domain while one or more Subnets reference it", nil)
	}

	stc, err := ddh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_DeleteDomain_FullMethodName, &corev1.DomainDeletionRequest{
		Id: &corev1.DomainId{Value: domain.ControllerDomainID.String()},
	}, nil, site.ID.String())
	if apiErr != nil && apiErr.Code != http.StatusNotFound {
		logAPIError(logger, apiErr, "failed to delete Domain via Core proxy")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	if apiErr != nil {
		logger.Warn().Str("domainID", domain.ID.String()).Str("controllerDomainID", domain.ControllerDomainID.String()).Msg("Domain not found in Core, removing stale REST projection")
	}

	domainDAO := cdbm.NewDomainDAO(ddh.dbSession)
	err = cdb.WithTx(ctx, ddh.dbSession, func(tx *cdb.Tx) error {
		return domainDAO.Delete(ctx, tx, domain.ID)
	})
	if err != nil {
		logger.Error().Err(err).Msg("failed to delete Domain from REST DB")
		return common.HandleTxError(c, logger, err, "Failed to delete Domain, DB transaction error")
	}

	return c.NoContent(http.StatusNoContent)
}

func getOwnedDomain(ctx context.Context, dbSession *cdb.Session, domainID, tenantID uuid.UUID) (*cdbm.Domain, error) {
	domains, _, err := cdbm.NewDomainDAO(dbSession).GetAll(ctx, nil, cdbm.DomainFilterInput{
		DomainIDs: []uuid.UUID{domainID},
		TenantIDs: []uuid.UUID{tenantID},
	}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		return nil, err
	}
	if len(domains) != 1 {
		return nil, cdb.ErrDoesNotExist
	}
	return &domains[0], nil
}

func getDomainSiteForTenant(
	ctx context.Context,
	logger zerolog.Logger,
	dbSession *cdb.Session,
	tenant *cdbm.Tenant,
	siteID string,
	requireRegistered bool,
) (*cdbm.Site, *cutil.APIError) {
	site, err := common.GetSiteFromIDString(ctx, nil, siteID, dbSession)
	if err != nil {
		if errors.Is(err, common.ErrInvalidID) || errors.Is(err, cdb.ErrDoesNotExist) {
			return nil, cutil.NewAPIError(http.StatusBadRequest, "Could not find Site with ID specified in request", nil)
		}
		logger.Error().Err(err).Msg("failed to retrieve Site from REST DB")
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Site, DB error", nil)
	}

	_, err = cdbm.NewTenantSiteDAO(dbSession).GetByTenantIDAndSiteID(ctx, nil, tenant.ID, site.ID, nil)
	if errors.Is(err, cdb.ErrDoesNotExist) {
		return nil, cutil.NewAPIError(http.StatusForbidden, "Tenant does not have access to Site", nil)
	}
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Tenant Site association")
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to determine Tenant access to Site, DB error", nil)
	}

	if requireRegistered && site.Status != cdbm.SiteStatusRegistered {
		return nil, cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Site %s is not in Registered state", site.ID), nil)
	}

	return site, nil
}
