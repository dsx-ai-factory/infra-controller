// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"cmp"
	"encoding/json"
	"net/http"
	"slices"

	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	domainOrderByFieldName = "name"
	domainOrderByNameAsc   = "NAME_ASC"
	domainOrderByNameDesc  = "NAME_DESC"
)

// CreateDNSDomainHandler creates a Domain (DNS zone) on a Site. Core owns Domains;
// REST passes the request through and keeps no copy.
type CreateDNSDomainHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewCreateDNSDomainHandler returns a Domain creation handler.
func NewCreateDNSDomainHandler(dbSession *cdb.Session, scp *sc.ClientPool) CreateDNSDomainHandler {
	return CreateDNSDomainHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle creates a Domain.
func (cdh CreateDNSDomainHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("DNSDomain", "Create", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiReq model.APIDNSDomainCreateRequest
	err := c.Bind(&apiReq)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	err = apiReq.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Domain create request data", err)
	}

	stc, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx: ctx, Logger: logger, DBSession: cdh.dbSession, SCP: cdh.scp, Org: org, User: dbUser, SiteID: apiReq.SiteID,
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	coreResp := &corev1.Domain{}
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_CreateDomain_FullMethodName, apiReq.ToProto(), coreResp, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to create Domain")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	return c.JSON(http.StatusCreated, model.NewAPIDNSDomain(coreResp, siteID))
}

// GetAllDNSDomainHandler lists the Domains on a Site.
type GetAllDNSDomainHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewGetAllDNSDomainHandler returns a Domain list handler.
func NewGetAllDNSDomainHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetAllDNSDomainHandler {
	return GetAllDNSDomainHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle lists Domains ordered by name.
func (gadh GetAllDNSDomainHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("DNSDomain", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiReq := model.APIDNSDomainGetAllRequest{}
	err := common.ValidateKnownQueryParams(c.QueryParams(), apiReq, pagination.PageRequest{})
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = c.Bind(&apiReq)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	err = apiReq.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Domain list request data", err)
	}

	pageRequest := pagination.PageRequest{}
	err = c.Bind(&pageRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}
	if pageRequest.OrderByStr == nil {
		pageRequest.OrderByStr = cutil.GetPtr(domainOrderByNameAsc)
	}
	err = pageRequest.Validate([]string{domainOrderByFieldName})
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	stc, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx: ctx, Logger: logger, DBSession: gadh.dbSession, SCP: gadh.scp, Org: org, User: dbUser, SiteID: apiReq.SiteID,
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	coreResp := &corev1.DomainList{}
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_FindDomain_FullMethodName, apiReq.ToProto(), coreResp, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to list Domains")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	// Core also returns the reverse zones it keeps for Network Segment
	// prefixes. They are not manageable Domains, so drop them before paging.
	domains := slices.DeleteFunc(coreResp.GetDomains(), func(domain *corev1.Domain) bool {
		return model.IsReverseDNSZoneName(domain.GetName())
	})

	// Core returns Domains unordered. Order before slicing so pages are stable.
	// Domain names are not unique, so the ID breaks ties, ascending in both
	// directions.
	slices.SortFunc(domains, func(a, b *corev1.Domain) int {
		order := cmp.Compare(a.GetName(), b.GetName())
		if *pageRequest.OrderByStr == domainOrderByNameDesc {
			order = -order
		}
		return cmp.Or(order, cmp.Compare(a.GetId().GetValue(), b.GetId().GetValue()))
	})
	total := len(domains)
	start := min(*pageRequest.Offset, total)
	end := min(start+*pageRequest.Limit, total)

	apiDomains := make([]*model.APIDNSDomain, 0, end-start)
	for _, domain := range domains[start:end] {
		apiDomains = append(apiDomains, model.NewAPIDNSDomain(domain, siteID))
	}

	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))
	return c.JSON(http.StatusOK, apiDomains)
}

// GetDNSDomainHandler retrieves one Domain on a Site.
type GetDNSDomainHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewGetDNSDomainHandler returns a Domain retrieval handler.
func NewGetDNSDomainHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetDNSDomainHandler {
	return GetDNSDomainHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle retrieves a Domain by ID. A deleted Domain is not found.
func (gdh GetDNSDomainHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("DNSDomain", "Get", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiReq := model.APIDNSDomainGetRequest{SiteID: c.QueryParam("siteId"), ID: c.Param("id")}
	err := common.ValidateKnownQueryParams(c.QueryParams(), apiReq)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = apiReq.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Domain get request data", err)
	}

	stc, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx: ctx, Logger: logger, DBSession: gdh.dbSession, SCP: gdh.scp, Org: org, User: dbUser, SiteID: apiReq.SiteID,
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	// Core has no get-by-ID call; a search by ID returns zero or one live Domain.
	coreResp := &corev1.DomainList{}
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_FindDomain_FullMethodName, apiReq.ToProto(), coreResp, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to retrieve Domain")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	// A reverse zone Core keeps for a Network Segment prefix is not a
	// manageable Domain, so it is not found here either.
	domains := coreResp.GetDomains()
	if len(domains) == 0 || model.IsReverseDNSZoneName(domains[0].GetName()) {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Domain with the specified ID", nil)
	}
	return c.JSON(http.StatusOK, model.NewAPIDNSDomain(domains[0], siteID))
}

// UpdateDNSDomainHandler updates a Domain on a Site.
type UpdateDNSDomainHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewUpdateDNSDomainHandler returns a Domain update handler.
func NewUpdateDNSDomainHandler(dbSession *cdb.Session, scp *sc.ClientPool) UpdateDNSDomainHandler {
	return UpdateDNSDomainHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle updates a Domain's default record TTL. Core rejects the update for a
// deleted Domain and for a reverse zone.
func (udh UpdateDNSDomainHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("DNSDomain", "Update", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiReq model.APIDNSDomainUpdateRequest
	err := c.Bind(&apiReq)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	apiReq.ID = c.Param("id")
	err = apiReq.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Domain update request data", err)
	}

	stc, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx: ctx, Logger: logger, DBSession: udh.dbSession, SCP: udh.scp, Org: org, User: dbUser, SiteID: apiReq.SiteID,
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	coreResp := &corev1.Domain{}
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_UpdateDomain_FullMethodName, apiReq.ToProto(), coreResp, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to update Domain")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	return c.JSON(http.StatusOK, model.NewAPIDNSDomain(coreResp, siteID))
}

// DeleteDNSDomainHandler deletes a Domain from a Site.
type DeleteDNSDomainHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewDeleteDNSDomainHandler returns a Domain deletion handler.
func NewDeleteDNSDomainHandler(dbSession *cdb.Session, scp *sc.ClientPool) DeleteDNSDomainHandler {
	return DeleteDNSDomainHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle deletes a Domain. Core rejects the deletion while a Subnet or
// Machine interface still uses the Domain, and for a reverse zone.
func (ddh DeleteDNSDomainHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("DNSDomain", "Delete", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiReq := model.APIDNSDomainDeleteRequest{SiteID: c.QueryParam("siteId"), ID: c.Param("id")}
	err := common.ValidateKnownQueryParams(c.QueryParams(), apiReq)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = apiReq.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Domain delete request data", err)
	}

	stc, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx: ctx, Logger: logger, DBSession: ddh.dbSession, SCP: ddh.scp, Org: org, User: dbUser, SiteID: apiReq.SiteID,
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_DeleteDomain_FullMethodName, apiReq.ToProto(), nil, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to delete Domain")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	return c.NoContent(http.StatusNoContent)
}
