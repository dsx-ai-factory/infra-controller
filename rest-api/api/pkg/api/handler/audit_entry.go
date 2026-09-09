// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strconv"

	goset "github.com/deckarep/golang-set/v2"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"go.opentelemetry.io/otel/attribute"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
)

// GetAllAuditEntryHandler is the API Handler for retrieving all AuditEntries
type GetAllAuditEntryHandler struct {
	dbSession *cdb.Session
}

// NewGetAllAuditEntryHandler initializes and returns a new handler for retrieving all AuditEntries
func NewGetAllAuditEntryHandler(dbSession *cdb.Session) GetAllAuditEntryHandler {
	return GetAllAuditEntryHandler{
		dbSession: dbSession,
	}
}

// Handle godoc
// @Summary Get all AuditEntries
// @Description Get all AuditEntries in the org
// @Tags audit
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Success 200 {array} []model.APIAuditEntry
// @Router /v2/org/{org}/nico/audit [get]
func (gaaeh GetAllAuditEntryHandler) Handle(c echo.Context) error {
	orgName, dbUser, ctx, logger, handlerSpan := common.SetupHandler("AuditEntry", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, orgName)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", orgName), nil)
	}

	// Validate role, only Provider or Tenant Admins are allowed to get audit log
	ok = auth.ValidateUserRoles(dbUser, orgName, nil, auth.ProviderAdminRole, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider/Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Admin role with org", nil)
	}

	// Validate paginantion request
	pageRequest := pagination.PageRequest{}
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate request attributes
	err = pageRequest.Validate(cdbm.AuditEntryOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// build filter
	filter := cdbm.AuditEntryFilterInput{OrgName: &orgName}

	// Check `failedOnly` in query
	failedOnly := false
	qpf := c.QueryParam("failedOnly")
	if qpf != "" {
		failedOnly, err = strconv.ParseBool(qpf)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `failedOnly` query param", nil)
		}
		filter.FailedOnly = &failedOnly
	}

	aeDAO := cdbm.NewAuditEntryDAO(gaaeh.dbSession)

	dbAuditEntries, total, err := aeDAO.GetAll(ctx, nil, filter, paginator.PageInput{Offset: pageRequest.Offset, Limit: pageRequest.Limit, OrderBy: pageRequest.OrderBy})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving all AuditEntries by param from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve AuditEntries", nil)
	}

	// build set of user IDs
	userIDs := goset.NewSet[uuid.UUID]()
	for _, dbae := range dbAuditEntries {
		if dbae.UserID != nil {
			userIDs.Add(*dbae.UserID)
		}
	}

	// build map of users
	dbUsersMap := make(map[uuid.UUID]*cdbm.User)
	userDAO := cdbm.NewUserDAO(gaaeh.dbSession)
	dbUsers, _, err := userDAO.GetAll(ctx, nil, cdbm.UserFilterInput{UserIDs: userIDs.ToSlice()},
		paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Users from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Users", nil)
	}
	for i := range dbUsers {
		dbUsersMap[dbUsers[i].ID] = &dbUsers[i]
	}

	// Create response
	apiAuditEntries := []model.APIAuditEntry{}
	for _, dbae := range dbAuditEntries {
		var dbu *cdbm.User
		if dbae.UserID != nil {
			dbu = dbUsersMap[*dbae.UserID]
		}
		apiae := model.NewAPIAuditEntry(dbae, dbu)

		apiAuditEntries = append(apiAuditEntries, apiae)
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

	return c.JSON(http.StatusOK, apiAuditEntries)
}

// GetAuditEntryHandler is the API Handler for getting individual Audit Entry
type GetAuditEntryHandler struct {
	dbSession *cdb.Session
}

// NewGetAuditEntryHandler initializes and returns a new handler for getting individual Audit Entry
func NewGetAuditEntryHandler(dbSession *cdb.Session) GetAuditEntryHandler {
	return GetAuditEntryHandler{
		dbSession: dbSession,
	}
}

// Handle godoc
// @Summary Get an AuditEntry
// @Description Get an individual AuditEntry for the org
// @Tags site
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Success 200 {object} model.APIAuditEntry
// @Router /v2/org/{org}/nico/audit/{id} [get]
func (gaeh GetAuditEntryHandler) Handle(c echo.Context) error {
	orgName, dbUser, ctx, logger, handlerSpan := common.SetupHandler("AuditEntry", "Get", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, orgName)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", orgName), nil)
	}

	// Validate role, only Provider or Tenant Admins are allowed to get audit log
	ok = auth.ValidateUserRoles(dbUser, orgName, nil, auth.ProviderAdminRole, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider/Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Admin role with org", nil)
	}

	// Get AuditEntry ID from URL param
	aeStrID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("audit_entry_id", aeStrID))

	aeID, err := uuid.Parse(aeStrID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Audit Entry ID in URL", nil)
	}

	// Get audit entry
	aeDAO := cdbm.NewAuditEntryDAO(gaeh.dbSession)
	dbAuditEntry, err := aeDAO.GetByID(ctx, nil, aeID)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Audit Entry with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving AuditEntry from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Audit Entry", nil)
	}

	// get user
	var auditUser *cdbm.User
	if dbAuditEntry.UserID != nil {
		userDAO := cdbm.NewUserDAO(gaeh.dbSession)
		if auditUser, err = userDAO.Get(ctx, nil, *dbAuditEntry.UserID, nil); err != nil {
			logger.Error().Err(err).Msgf("error retrieving User %s from DB", *dbAuditEntry.UserID)
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve User", nil)
		}
	}

	// Create response
	apiAuditEntry := model.NewAPIAuditEntry(*dbAuditEntry, auditUser)

	return c.JSON(http.StatusOK, apiAuditEntry)
}
