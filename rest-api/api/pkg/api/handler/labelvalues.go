// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"errors"
	"net/http"
	"net/url"
	"slices"

	mapset "github.com/deckarep/golang-set/v2"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	modelutil "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
)

type labelListKind string

const (
	labelListKeys   labelListKind = "keys"
	labelListValues labelListKind = "values"

	labelKeyOrderByDefault   = "KEY_ASC"
	labelValueOrderByDefault = "VALUE_ASC"
)

// getLabelKey decodes and validates the label key path parameter.
func getLabelKey(c echo.Context) (string, error) {
	labelKey, err := url.PathUnescape(c.Param("key"))
	if err != nil {
		return "", err
	}
	err = modelutil.ValidateLabels(map[string]string{labelKey: ""})
	if err != nil {
		return "", err
	}
	return labelKey, nil
}

// GetAllExpectedMachineLabelKeyHandler lists ExpectedMachine label keys.
type GetAllExpectedMachineLabelKeyHandler struct {
	dbSession  *cdb.Session
	tracerSpan *cutil.TracerSpan
}

// NewGetAllExpectedMachineLabelKeyHandler initializes an ExpectedMachine label-key list handler.
func NewGetAllExpectedMachineLabelKeyHandler(dbSession *cdb.Session) GetAllExpectedMachineLabelKeyHandler {
	return GetAllExpectedMachineLabelKeyHandler{dbSession: dbSession, tracerSpan: cutil.NewTracerSpan()}
}

// Handle godoc
// @Summary Get all ExpectedMachine label keys
// @Tags ExpectedMachine
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string false "ID of Site"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Label key ordering" Enums(KEY_ASC, KEY_DESC) default(KEY_ASC)
// @Success 200 {array} string
// @Router /v2/org/{org}/nico/expected-machine/label/key [get]
func (h GetAllExpectedMachineLabelKeyHandler) Handle(c echo.Context) error {
	return handleExpectedMachineLabelList(c, h.dbSession, h.tracerSpan, labelListKeys)
}

// GetAllExpectedMachineLabelValueHandler lists values for an ExpectedMachine label key.
type GetAllExpectedMachineLabelValueHandler struct {
	dbSession  *cdb.Session
	tracerSpan *cutil.TracerSpan
}

// NewGetAllExpectedMachineLabelValueHandler initializes an ExpectedMachine label-value list handler.
func NewGetAllExpectedMachineLabelValueHandler(dbSession *cdb.Session) GetAllExpectedMachineLabelValueHandler {
	return GetAllExpectedMachineLabelValueHandler{dbSession: dbSession, tracerSpan: cutil.NewTracerSpan()}
}

// Handle godoc
// @Summary Get all values for an ExpectedMachine label key
// @Tags ExpectedMachine
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param key path string true "Label key"
// @Param siteId query string false "ID of Site"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Label value ordering" Enums(VALUE_ASC, VALUE_DESC) default(VALUE_ASC)
// @Success 200 {array} string
// @Router /v2/org/{org}/nico/expected-machine/label/key/{key}/value [get]
func (h GetAllExpectedMachineLabelValueHandler) Handle(c echo.Context) error {
	return handleExpectedMachineLabelList(c, h.dbSession, h.tracerSpan, labelListValues)
}

func handleExpectedMachineLabelList(c echo.Context, dbSession *cdb.Session, tracerSpan *cutil.TracerSpan, kind labelListKind) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedMachineLabel", "GetAll", c, tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	provider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, dbSession, org, dbUser, true, nil)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	apiRequest, pageRequest, labelKey, requestError := bindLabelListRequest(c, kind)
	if requestError != nil {
		return cutil.NewAPIErrorResponse(c, requestError.Code, requestError.Message, requestError.Data)
	}

	siteIDs := mapset.NewSet[uuid.UUID]()
	if provider != nil {
		sites, _, err := cdbm.NewSiteDAO(dbSession).GetAll(ctx, nil,
			cdbm.SiteFilterInput{InfrastructureProviderIDs: []uuid.UUID{provider.ID}},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Sites from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Sites for org due to DB error", nil)
		}
		for _, site := range sites {
			siteIDs.Add(site.ID)
		}
	}
	if tenant != nil {
		privilegedSiteIDs, err := common.GetPrivilegedAccessSiteIDsForTenant(ctx, nil, dbSession, tenant)
		if err != nil {
			logger.Error().Err(err).Msg("error resolving privileged Site access for Tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to resolve Tenant capability due to DB error", nil)
		}
		for _, siteID := range privilegedSiteIDs {
			siteIDs.Add(siteID)
		}
	}
	if provider == nil && tenant != nil && siteIDs.Cardinality() == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have Targeted Instance Creation capability enabled for any Site", nil)
	}

	if apiRequest.SiteID != "" {
		site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, dbSession)
		if err != nil {
			if errors.Is(err, common.ErrInvalidID) || errors.Is(err, cdb.ErrDoesNotExist) {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Site specified in query", nil)
			}
			logger.Error().Err(err).Str("site_id", apiRequest.SiteID).Msg("error retrieving Site specified in query")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in query", nil)
		}
		if !siteIDs.Contains(site.ID) {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site specified in query", nil)
		}
		siteIDs.Clear()
		siteIDs.Add(site.ID)
	}

	dao := cdbm.NewExpectedMachineDAO(dbSession)
	filter := cdbm.ExpectedMachineFilterInput{SiteIDs: siteIDs.ToSlice()}
	var items []string
	var total int
	var err error
	if kind == labelListKeys {
		items, total, err = dao.GetDistinctLabelKeys(ctx, nil, filter, pageRequest.ConvertToDB())
	} else {
		items, total, err = dao.GetDistinctLabelValues(ctx, nil, labelKey, filter, pageRequest.ConvertToDB())
	}
	if err != nil {
		logger.Error().Err(err).Str("label_list_kind", string(kind)).Msg("error retrieving ExpectedMachine labels from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve ExpectedMachine labels due to DB error", nil)
	}
	return writeLabelListResponse(c, logger, items, total, pageRequest)
}

// GetAllMachineLabelKeyHandler lists Machine label keys.
type GetAllMachineLabelKeyHandler struct {
	dbSession  *cdb.Session
	tracerSpan *cutil.TracerSpan
}

// NewGetAllMachineLabelKeyHandler initializes a Machine label-key list handler.
func NewGetAllMachineLabelKeyHandler(dbSession *cdb.Session) GetAllMachineLabelKeyHandler {
	return GetAllMachineLabelKeyHandler{dbSession: dbSession, tracerSpan: cutil.NewTracerSpan()}
}

// Handle godoc
// @Summary Get all Machine label keys
// @Tags Machine
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string false "ID of Site"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Label key ordering" Enums(KEY_ASC, KEY_DESC) default(KEY_ASC)
// @Success 200 {array} string
// @Router /v2/org/{org}/nico/machine/label/key [get]
func (h GetAllMachineLabelKeyHandler) Handle(c echo.Context) error {
	return handleMachineLabelList(c, h.dbSession, h.tracerSpan, labelListKeys)
}

// GetAllMachineLabelValueHandler lists values for a Machine label key.
type GetAllMachineLabelValueHandler struct {
	dbSession  *cdb.Session
	tracerSpan *cutil.TracerSpan
}

// NewGetAllMachineLabelValueHandler initializes a Machine label-value list handler.
func NewGetAllMachineLabelValueHandler(dbSession *cdb.Session) GetAllMachineLabelValueHandler {
	return GetAllMachineLabelValueHandler{dbSession: dbSession, tracerSpan: cutil.NewTracerSpan()}
}

// Handle godoc
// @Summary Get all values for a Machine label key
// @Tags Machine
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param key path string true "Label key"
// @Param siteId query string false "ID of Site"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Label value ordering" Enums(VALUE_ASC, VALUE_DESC) default(VALUE_ASC)
// @Success 200 {array} string
// @Router /v2/org/{org}/nico/machine/label/key/{key}/value [get]
func (h GetAllMachineLabelValueHandler) Handle(c echo.Context) error {
	return handleMachineLabelList(c, h.dbSession, h.tracerSpan, labelListValues)
}

func handleMachineLabelList(c echo.Context, dbSession *cdb.Session, tracerSpan *cutil.TracerSpan, kind labelListKind) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineLabel", "GetAll", c, tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	provider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, dbSession, org, dbUser, true, nil)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	apiRequest, pageRequest, labelKey, requestError := bindLabelListRequest(c, kind)
	if requestError != nil {
		return cutil.NewAPIErrorResponse(c, requestError.Code, requestError.Message, requestError.Data)
	}

	filter := cdbm.MachineFilterInput{}
	var err error
	if provider != nil {
		filter.InfrastructureProviderIDs = []uuid.UUID{provider.ID}
	}
	var privilegedSiteIDs []uuid.UUID
	if tenant != nil {
		privilegedSiteIDs, err = common.GetPrivilegedAccessSiteIDsForTenant(ctx, nil, dbSession, tenant)
		if err != nil {
			logger.Error().Err(err).Msg("error resolving privileged Site access for Tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to verify privileges for Tenant", nil)
		}
		if len(privilegedSiteIDs) == 0 && provider == nil {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have Targeted Instance Creation capability enabled for any Site", nil)
		}
	}

	if apiRequest.SiteID != "" {
		site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, dbSession)
		if err != nil {
			if errors.Is(err, common.ErrInvalidID) || errors.Is(err, cdb.ErrDoesNotExist) {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Site specified in query", nil)
			}
			logger.Error().Err(err).Str("site_id", apiRequest.SiteID).Msg("error retrieving Site specified in query")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in query", nil)
		}
		providerDenied := provider != nil && site.InfrastructureProviderID != provider.ID
		tenantDenied := provider == nil && !slices.Contains(privilegedSiteIDs, site.ID)
		if providerDenied || tenantDenied {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site specified in query", nil)
		}
		filter.SiteIDs = []uuid.UUID{site.ID}
	} else if tenant != nil && provider == nil {
		filter.SiteIDs = privilegedSiteIDs
	}

	dao := cdbm.NewMachineDAO(dbSession)
	var items []string
	var total int
	if kind == labelListKeys {
		items, total, err = dao.GetDistinctLabelKeys(ctx, nil, filter, pageRequest.ConvertToDB())
	} else {
		items, total, err = dao.GetDistinctLabelValues(ctx, nil, labelKey, filter, pageRequest.ConvertToDB())
	}
	if err != nil {
		logger.Error().Err(err).Str("label_list_kind", string(kind)).Msg("error retrieving Machine labels from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Machine labels due to DB error", nil)
	}
	return writeLabelListResponse(c, logger, items, total, pageRequest)
}

func bindLabelListRequest(c echo.Context, kind labelListKind) (model.APILabelGetAllRequest, pagination.PageRequest, string, *cutil.APIError) {
	apiRequest := model.APILabelGetAllRequest{}
	pageRequest := pagination.PageRequest{}
	err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest, pageRequest)
	if err != nil {
		return apiRequest, pageRequest, "", cutil.NewAPIError(http.StatusBadRequest, err.Error(), nil)
	}
	err = c.Bind(&apiRequest)
	if err != nil {
		return apiRequest, pageRequest, "", cutil.NewAPIError(http.StatusBadRequest, "Failed to parse request data", nil)
	}
	err = c.Bind(&pageRequest)
	if err != nil {
		return apiRequest, pageRequest, "", cutil.NewAPIError(http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	labelKey := ""
	orderFields := cdbm.LabelKeyOrderByFields
	defaultOrder := labelKeyOrderByDefault
	if kind == labelListValues {
		labelKey, err = getLabelKey(c)
		if err != nil {
			return apiRequest, pageRequest, "", cutil.NewAPIError(http.StatusBadRequest, "Invalid label key", err)
		}
		orderFields = cdbm.LabelValueOrderByFields
		defaultOrder = labelValueOrderByDefault
	}
	if pageRequest.OrderByStr == nil {
		pageRequest.OrderByStr = cutil.GetPtr(defaultOrder)
	}
	err = pageRequest.Validate(orderFields)
	if err != nil {
		return apiRequest, pageRequest, "", cutil.NewAPIError(http.StatusBadRequest, "Failed to validate pagination request data", err)
	}
	return apiRequest, pageRequest, labelKey, nil
}

func writeLabelListResponse(c echo.Context, logger zerolog.Logger, items []string, total int, pageRequest pagination.PageRequest) error {
	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))
	return c.JSON(http.StatusOK, items)
}
