// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"time"

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
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewCreateDomainHandler returns a Domain creation handler.
func NewCreateDomainHandler(dbSession *cdb.Session, scp *sc.ClientPool) CreateDomainHandler {
	return CreateDomainHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle creates a tenant-owned DNS Domain.
func (cdh CreateDomainHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Domain", "Create", c)
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

	// Commit the owner-to-reserved-ID mapping before ANY Core operation. Even a
	// proxy timeout can conceal a successfully executed Core write.
	domainDAO := cdbm.NewDomainDAO(cdh.dbSession)
	coreID := uuid.New()
	inserted := false
	domain, err := cdb.WithTxResult(ctx, cdh.dbSession, func(tx *cdb.Tx) (*cdbm.Domain, error) {
		row, fresh, reserveErr := domainDAO.ReserveOwned(ctx, tx, cdbm.DomainCreateInput{
			Hostname: apiRequest.Name, Org: org, TenantID: &tenant.ID, SiteID: &site.ID,
			ControllerDomainID: &coreID, Status: cdbm.DomainStatusPending, CreatedBy: dbUser.ID,
		})
		inserted = fresh
		return row, reserveErr
	})
	if err != nil {
		logger.Error().Err(err).Msg("failed to reserve owned Domain in REST DB")
		return common.HandleTxError(c, logger, err, "Failed to reserve Domain, DB transaction error")
	}
	if domain.ControllerDomainID == nil || domain.SiteID == nil || domain.TenantID == nil ||
		*domain.SiteID != site.ID || *domain.TenantID != tenant.ID ||
		domain.Hostname != cdbm.NormalizeForwardDomainName(apiRequest.Name) || domain.Org != org {
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Domain reservation does not match the requested owner and name", nil)
	}
	if !inserted {
		switch domain.Status {
		case cdbm.DomainStatusReady:
			return c.JSON(http.StatusOK, model.NewAPIDomain(domain))
		case cdbm.DomainStatusPending, cdbm.DomainStatusRegistering, cdbm.DomainStatusRejecting:
			return c.JSON(http.StatusAccepted, model.NewAPIDomain(domain))
		default:
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Domain reservation is not available for creation", nil)
		}
	}
	coreRequest := apiRequest.ToProto()
	coreRequest.Name = domain.Hostname
	coreRequest.ReservedId = &corev1.DomainId{Value: domain.ControllerDomainID.String()}
	coreDomain := &corev1.Domain{}
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_CreateDomain_FullMethodName, coreRequest, coreDomain, site.ID.String())
	if apiErr != nil {
		logAPIError(logger, apiErr, "Domain create did not return a confirmed resource; reservation retained")
		if apiErr.Code == http.StatusConflict || apiErr.Code == http.StatusBadRequest || apiErr.Code == http.StatusPreconditionFailed {
			// These definitive Core validation/conflict responses may be surfaced;
			// retain a durable Error row so a retry cannot adopt by DNS name.
			staged, transitionErr := cdb.WithTxResult(ctx, cdh.dbSession, func(tx *cdb.Tx) (bool, error) {
				return domainDAO.StageRejectedOwned(ctx, tx, domain.ID, *domain.ControllerDomainID, nil)
			})
			if transitionErr == nil && staged {
				domain.Status = cdbm.DomainStatusRejecting
				// A lost cancel reply leaves Rejecting durable. A delayed successful
				// create cannot mark it Ready; the periodic recovery retries cancel.
				cancelErr := common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_DeleteDomain_FullMethodName,
					&corev1.DomainDeletionRequest{Id: &corev1.DomainId{Value: domain.ControllerDomainID.String()}, CancelReservedId: true}, nil, site.ID.String())
				if cancelErr == nil {
					finalized, err := cdb.WithTxResult(ctx, cdh.dbSession, func(tx *cdb.Tx) (bool, error) {
						return domainDAO.TransitionOwned(ctx, tx, domain.ID, *domain.ControllerDomainID, cdbm.DomainStatusRejecting, cdbm.DomainStatusError)
					})
					if err == nil && finalized {
						return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
					}
					logger.Error().Err(err).Str("domainID", domain.ID.String()).Msg("Domain cancellation confirmed but Error projection unresolved")
				}
				return c.JSON(http.StatusAccepted, model.NewAPIDomain(domain))
			}
			logger.Error().Err(transitionErr).Str("domainID", domain.ID.String()).Msg("could not stage rejected Domain intent")
		}
		// A 504 does not cancel an in-flight Site workflow. Keep the durable
		// reservation rather than reporting a false rollback or issuing a new ID.
		return c.JSON(http.StatusAccepted, model.NewAPIDomain(domain))
	}
	if coreDomain.GetId().GetValue() != domain.ControllerDomainID.String() || coreDomain.GetName() != domain.Hostname {
		logger.Error().Str("domainID", domain.ID.String()).Msg("Core returned a different identity for reserved Domain")
		return c.JSON(http.StatusAccepted, model.NewAPIDomain(domain))
	}
	changed, err := cdb.WithTxResult(ctx, cdh.dbSession, func(tx *cdb.Tx) (bool, error) {
		return domainDAO.TransitionOwned(ctx, tx, domain.ID, *domain.ControllerDomainID, cdbm.DomainStatusPending, cdbm.DomainStatusReady)
	})
	if err != nil || !changed {
		logger.Error().Err(err).Str("domainID", domain.ID.String()).Msg("Domain ready transition did not commit")
		return c.JSON(http.StatusAccepted, model.NewAPIDomain(domain))
	}
	domain.Status = cdbm.DomainStatusReady
	return c.JSON(http.StatusCreated, model.NewAPIDomain(domain))
}

// GetAllDomainHandler lists tenant-owned DNS Domains.
type GetAllDomainHandler struct {
	dbSession *cdb.Session
}

// NewGetAllDomainHandler returns a Domain list handler.
func NewGetAllDomainHandler(dbSession *cdb.Session) GetAllDomainHandler {
	return GetAllDomainHandler{
		dbSession: dbSession,
	}
}

// Handle lists tenant-owned DNS Domains, optionally limited to one authorized Site.
func (gadh GetAllDomainHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Domain", "GetAll", c)
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
	} else {
		// A tenant may retain Domain projections after losing access to a Site.
		// Do not reveal those Domains through the unfiltered list endpoint.
		tenantSites, _, err := cdbm.NewTenantSiteDAO(gadh.dbSession).GetAll(ctx, nil,
			cdbm.TenantSiteFilterInput{TenantIDs: []uuid.UUID{tenant.ID}},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Tenant Site associations for Domain list")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve accessible Sites, DB error", nil)
		}
		for _, tenantSite := range tenantSites {
			filter.SiteIDs = append(filter.SiteIDs, tenantSite.SiteID)
		}
	}

	// An empty Site filter means no Sites are accessible, not all Sites.
	domains := []cdbm.Domain{}
	total := 0
	if len(filter.SiteIDs) > 0 {
		domains, total, err = cdbm.NewDomainDAO(gadh.dbSession).GetAll(ctx, nil, filter, pageRequest.ConvertToDB(), nil)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Domains from REST DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Domains, DB error", nil)
		}
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
	dbSession *cdb.Session
}

// NewGetDomainHandler returns a single-Domain retrieval handler.
func NewGetDomainHandler(dbSession *cdb.Session) GetDomainHandler {
	return GetDomainHandler{
		dbSession: dbSession,
	}
}

// Handle retrieves one tenant-owned DNS Domain.
func (gdh GetDomainHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Domain", "Get", c)
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
	if domain.SiteID == nil || *domain.SiteID == uuid.Nil {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Domain with the specified ID", nil)
	}
	_, apiErr = getDomainSiteForTenant(ctx, logger, gdh.dbSession, tenant, domain.SiteID.String(), false)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	return c.JSON(http.StatusOK, model.NewAPIDomain(domain))
}

// DeleteDomainHandler deletes a tenant-owned DNS Domain from Core and its REST projection.
type DeleteDomainHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewDeleteDomainHandler returns a Domain deletion handler.
func NewDeleteDomainHandler(dbSession *cdb.Session, scp *sc.ClientPool) DeleteDomainHandler {
	return DeleteDomainHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle deletes a tenant-owned DNS Domain.
func (ddh DeleteDomainHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Domain", "Delete", c)
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

	// Commit Deleting before contacting Core. This blocks new REST subnet
	// references and leaves a recoverable owner/Core-ID mapping if a proxy
	// times out, the API process exits, or the final REST write fails. Keep
	// recovery outside the handler's complete RPC window, including on retries.
	domainDAO := cdbm.NewDomainDAO(ddh.dbSession)
	if domain.Status != cdbm.DomainStatusReady && domain.Status != cdbm.DomainStatusPending && domain.Status != cdbm.DomainStatusRejecting && domain.Status != cdbm.DomainStatusError && domain.Status != cdbm.DomainStatusDeleting {
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Domain is not available for deletion", nil)
	}
	changed, transitionErr := cdb.WithTxResult(ctx, ddh.dbSession, func(tx *cdb.Tx) (bool, error) {
		return domainDAO.ReserveDeletionOwned(ctx, tx, domain.ID, *domain.ControllerDomainID, domain.Status, 90*time.Second)
	})
	if transitionErr != nil {
		return common.HandleTxError(c, logger, transitionErr, "Failed to reserve Domain deletion, DB transaction error")
	}
	if !changed {
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Domain changed while reserving deletion", nil)
	}
	restoreOnRefusal := domain.Status == cdbm.DomainStatusReady

	// Cancellation creates a terminal Core tombstone if a late reserved-ID
	// create has not yet arrived. A plain not-found delete cannot close that
	// race, and a timeout is NEVER interpreted as successful deletion.
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_DeleteDomain_FullMethodName, &corev1.DomainDeletionRequest{
		Id: &corev1.DomainId{Value: domain.ControllerDomainID.String()}, CancelReservedId: true,
	}, nil, site.ID.String())
	if apiErr != nil {
		if apiErr.Code == http.StatusPreconditionFailed {
			// Core definitively refused deletion while the Domain is referenced.
			// Only this request's fresh Ready->Deleting reservation can be
			// restored. A retried/older deletion may still have an RPC in flight.
			if restoreOnRefusal {
				restored, restoreErr := cdb.WithTxResult(ctx, ddh.dbSession, func(tx *cdb.Tx) (bool, error) {
					return domainDAO.RestoreRejectedDeletion(ctx, tx, domain.ID, *domain.ControllerDomainID)
				})
				if restoreErr != nil || !restored {
					logger.Error().Err(restoreErr).Bool("restored", restored).Str("domainID", domain.ID.String()).Msg("could not restore Domain after referenced-delete rejection")
					return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to restore Domain after deletion was rejected", nil)
				}
			}
			return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
		}
		logAPIError(logger, apiErr, "Domain deletion is unconfirmed; durable Deleting reservation retained")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	err = cdb.WithTx(ctx, ddh.dbSession, func(tx *cdb.Tx) error {
		return domainDAO.Delete(ctx, tx, domain.ID)
	})
	if err != nil {
		logger.Error().Err(err).Str("domainID", domain.ID.String()).Msg("Core deletion confirmed but REST projection still Deleting")
		return common.HandleTxError(c, logger, err, "Failed to finalize Domain deletion, DB transaction error")
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
