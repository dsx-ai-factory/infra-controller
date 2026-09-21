// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"reflect"
	"time"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"go.opentelemetry.io/otel/attribute"
	tclient "go.temporal.io/sdk/client"

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
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllSkuHandler is the API Handler for getting all SKUs
type GetAllSkuHandler struct {
	dbSession *cdb.Session
	tc        tclient.Client
	cfg       *config.Config
}

// NewGetAllSkuHandler initializes and returns a new handler for getting all SKUs
func NewGetAllSkuHandler(dbSession *cdb.Session, tc tclient.Client, cfg *config.Config) GetAllSkuHandler {
	return GetAllSkuHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get all SKUs
// @Description Get all SKUs
// @Tags SKU
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string false "ID of Site (optional, filters results to specific site)"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APISku
// @Router /v2/org/{org}/nico/sku [get]
func (gash GetAllSkuHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("GetAll", "SKU", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org membership
	if _, err := dbUser.OrgData.GetOrgByName(org); err != nil {
		logger.Warn().Msg("could not validate org membership for user, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Get Site ID from query param - REQUIRED
	siteIDStr := c.QueryParam("siteId")
	if siteIDStr == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site ID must be specified in query parameter 'siteId'", nil)
	}

	site, err := common.GetSiteFromIDString(ctx, nil, siteIDStr, gash.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request data does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request data, DB error", nil)
	}

	// Scope tenant privilege to the Site whose SKUs are requested.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, gash.dbSession, org, dbUser, true, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate based on whether user is provider or tenant
	if infrastructureProvider != nil {
		// Validate that site belongs to the organization's infrastructure provider
		if site.InfrastructureProviderID != infrastructureProvider.ID {
			logger.Warn().Msg("Site specified in request data does not belong to current org's Infrastructure Provider")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request data is does not belong to current org", nil)
		}
	} else if tenant != nil {
		enabled, err := common.TenantHasTargetedInstanceCreation(ctx, nil, gash.dbSession, tenant, &common.TenantPrivilegeScope{SiteID: &site.ID})
		if err != nil {
			logger.Error().Err(err).Msg("error resolving TargetedInstanceCreation for Tenant/Site")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to resolve Tenant capability for Site due to DB error", nil)
		}
		if !enabled {
			logger.Warn().Msg("Tenant doesn't have targeted Instance creation capability for Site, access denied")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant must have targeted Instance creation capability in order to retrieve SKUs", nil)
		}
	}

	filterInput := cdbm.SkuFilterInput{
		SiteIDs: []uuid.UUID{site.ID},
	}

	// Validate pagination request
	pageRequest := pagination.PageRequest{}
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate pagination attributes
	err = pageRequest.Validate(cdbm.SkuOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Get SKUs from DB
	skuDAO := cdbm.NewSkuDAO(gash.dbSession)
	skus, total, err := skuDAO.GetAll(
		ctx,
		nil,
		filterInput,
		paginator.PageInput{
			Offset:  pageRequest.Offset,
			Limit:   pageRequest.Limit,
			OrderBy: pageRequest.OrderBy,
		},
	)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving SKUs from db")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SKUs, DB error", nil)
	}

	// Create response
	apiSkus := []*model.APISku{}
	for _, sku := range skus {
		apiSku := model.NewAPISku(&sku)
		apiSkus = append(apiSkus, apiSku)
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

	return c.JSON(http.StatusOK, apiSkus)
}

// ~~~~~ Get Handler ~~~~~ //

// GetSkuHandler is the API Handler for retrieving SKU
type GetSkuHandler struct {
	dbSession *cdb.Session
	tc        tclient.Client
	cfg       *config.Config
}

// NewGetSkuHandler initializes and returns a new handler to retrieve SKU
func NewGetSkuHandler(dbSession *cdb.Session, tc tclient.Client, cfg *config.Config) GetSkuHandler {
	return GetSkuHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Retrieve the SKU
// @Description Retrieve the SKU by ID
// @Tags SKU
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of SKU"
// @Success 200 {object} model.APISku
// @Router /v2/org/{org}/nico/sku/{id} [get]
func (gsh GetSkuHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Get", "SKU", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org membership
	if _, err := dbUser.OrgData.GetOrgByName(org); err != nil {
		logger.Warn().Msg("could not validate org membership for user, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Get SKU ID from URL param
	skuID := c.Param("id")

	logger = logger.With().Str("SKU ID", skuID).Logger()

	cotel.SetAttribute(handlerSpan, attribute.String("sku_id", skuID))

	// Get SKU from DB by ID
	skuDAO := cdbm.NewSkuDAO(gsh.dbSession)
	sku, err := skuDAO.Get(ctx, nil, skuID)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find SKU with ID: %s", skuID), nil)
		}
		logger.Error().Err(err).Msg("error retrieving SKU from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SKU, DB error", nil)
	}

	// Get Site for the SKU
	siteDAO := cdbm.NewSiteDAO(gsh.dbSession)
	site, err := siteDAO.GetByID(ctx, nil, sku.SiteID, nil, false)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for SKU, DB error", nil)
	}

	// Scope tenant privilege to the SKU's Site.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, gsh.dbSession, org, dbUser, true, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate based on whether user is provider or tenant
	if infrastructureProvider != nil {
		// Validate that site belongs to the organization's infrastructure provider
		if site.InfrastructureProviderID != infrastructureProvider.ID {
			logger.Warn().Msg("SKU does not belong to a Site owned by org's Infrastructure Provider")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "SKU does not belong to a Site owned by current org", nil)
		}
	} else if tenant != nil {
		enabled, err := common.TenantHasTargetedInstanceCreation(ctx, nil, gsh.dbSession, tenant, &common.TenantPrivilegeScope{SiteID: &site.ID})
		if err != nil {
			logger.Error().Err(err).Msg("error resolving TargetedInstanceCreation for Tenant/Site")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to resolve Tenant capability for Site due to DB error", nil)
		}
		if !enabled {
			logger.Warn().Msg("Tenant doesn't have targeted Instance creation capability for Site, access denied")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant must have targeted Instance creation capability in order to retrieve SKU", nil)
		}
	}

	// Create response
	apiSku := model.NewAPISku(sku)

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiSku)
}

// CreateSkuHandler creates one SKU on a Site's Core service.
type CreateSkuHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewCreateSkuHandler initializes and returns a new CreateSkuHandler.
func NewCreateSkuHandler(dbSession *cdb.Session, scp *sc.ClientPool) CreateSkuHandler {
	return CreateSkuHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle godoc
// @Summary Create SKU
// @Description Create a SKU on the selected Site's Core service.
// @Tags SKU
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param request body model.APISkuCreateRequest true "SKU create request"
// @Success 201 {object} model.APISku
// @Router /v2/org/{org}/nico/sku [post]
func (csh CreateSkuHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("SKU", "Create", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Validate request data
	// Bind request data to API model
	apiReq := model.APISkuCreateRequest{}
	err := c.Bind(&apiReq)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiReq.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating SKU create request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating SKU create request data", verr)
	}

	stc, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx:       ctx,
		Logger:    logger,
		DBSession: csh.dbSession,
		SCP:       csh.scp,
		Org:       org,
		User:      dbUser,
		SiteID:    apiReq.SiteID,
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	logger.Info().Str("skuID", apiReq.ID).Str("siteID", siteID).Msg("creating SKU via Core proxy")
	var ids corev1.SkuIdList
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_CreateSku_FullMethodName, apiReq.ToProto(), &ids, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to create SKU via Core proxy")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	if len(ids.Ids) != 1 {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Core returned an unexpected SKU create response", nil)
	}

	var response corev1.SkuList
	apiErr = common.ExecuteCoreGRPC(
		ctx,
		stc,
		corev1.Forge_FindSkusByIds_FullMethodName,
		&corev1.SkusByIdsRequest{Ids: []string{ids.Ids[0]}},
		&response,
		siteID,
	)
	if apiErr == nil && len(response.Skus) != 1 {
		apiErr = cutil.NewAPIError(http.StatusNotFound, "Could not find SKU with the specified ID", nil)
	}

	var skuToPersist *corev1.Sku
	if apiErr != nil {
		logger.Warn().Err(apiErr).Str("skuID", ids.Ids[0]).Str("siteID", siteID).
			Msg("SKU created but post-create retrieval failed; returning request-derived response")
		createdSKU := apiReq.ToProto().Skus[0]
		createdSKU.Id = ids.Ids[0]
		skuToPersist = createdSKU
	} else {
		skuToPersist = response.Skus[0]
	}

	if skuToPersist == nil || skuToPersist.Id == "" {
		err = errors.New("cannot persist an empty Core SKU")
	} else {
		projected := &cdbm.SKU{}
		projected.FromProto(skuToPersist, uuid.MustParse(siteID))
		var created *time.Time
		if !projected.Created.IsZero() {
			created = &projected.Created
		}
		skuDAO := cdbm.NewSkuDAO(csh.dbSession)
		err = cdb.WithTx(ctx, csh.dbSession, func(tx *cdb.Tx) error {
			err := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(projected.ID), nil)
			if err != nil {
				return err
			}

			_, err = skuDAO.Create(ctx, tx, cdbm.SkuCreateInput{
				SkuID:                projected.ID,
				SiteID:               projected.SiteID,
				Created:              created,
				Description:          projected.Description,
				SchemaVersion:        projected.SchemaVersion,
				Components:           projected.Components,
				DeviceType:           projected.DeviceType,
				AssociatedMachineIds: projected.AssociatedMachineIds,
			})
			return err
		})
	}
	if err != nil {
		errorChecker := cdb.PostgresErrorChecker{}
		if errorChecker.IsUniqueConstraintError(err) {
			message := fmt.Sprintf(
				"SKU %q already exists; inspect it with GET /v2/org/%s/nico/sku/%s and retry changes as an update",
				skuToPersist.Id,
				org,
				skuToPersist.Id,
			)
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, message, nil)
		}
		logger.Error().Err(err).Str("skuID", ids.Ids[0]).Str("siteID", siteID).
			Msg("SKU created in Core but failed to update REST DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "SKU was created in Core but failed to update REST DB", nil)
	}
	if apiErr != nil {
		return c.JSON(http.StatusCreated, model.NewAPISkuFromCreateRequest(apiReq, ids.Ids[0], siteID))
	}
	return c.JSON(http.StatusCreated, model.NewAPISkuFromProto(skuToPersist, siteID))
}

// UpdateSkuHandler partially updates one SKU on a Site's Core service.
type UpdateSkuHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewUpdateSkuHandler returns a new UpdateSkuHandler.
func NewUpdateSkuHandler(dbSession *cdb.Session, scp *sc.ClientPool) UpdateSkuHandler {
	return UpdateSkuHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle godoc
// @Summary Update SKU
// @Description Update selected mutable fields on a SKU.
// @Tags SKU
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "SKU ID"
// @Param request body model.APISkuUpdateRequest true "SKU update request"
// @Success 200 {object} model.APISku
// @Router /v2/org/{org}/nico/sku/{id} [patch]
func (ush UpdateSkuHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("SKU", "Update", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	skuID := c.Param("id")
	if skuID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "SKU ID must be specified", nil)
	}
	cotel.SetAttribute(handlerSpan, attribute.String("sku_id", skuID))

	apiReq := model.APISkuUpdateRequest{}
	err := c.Bind(&apiReq)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	err = apiReq.Validate()
	if err != nil {
		logger.Warn().Err(err).Msg("error validating SKU update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating SKU update request data", err)
	}

	skuDAO := cdbm.NewSkuDAO(ush.dbSession)
	savedSKU, err := skuDAO.Get(ctx, nil, skuID)
	if errors.Is(err, cdb.ErrDoesNotExist) {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find SKU with the specified ID", nil)
	}
	if err != nil {
		logger.Error().Err(err).Str("skuID", skuID).Msg("error retrieving SKU from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SKU, DB error", nil)
	}

	stc, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx:       ctx,
		Logger:    logger,
		DBSession: ush.dbSession,
		SCP:       ush.scp,
		Org:       org,
		User:      dbUser,
		SiteID:    savedSKU.SiteID.String(),
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	var response corev1.SkuList
	apiErr = common.ExecuteCoreGRPC(
		ctx,
		stc,
		corev1.Forge_FindSkusByIds_FullMethodName,
		&corev1.SkusByIdsRequest{Ids: []string{skuID}},
		&response,
		siteID,
	)
	if apiErr == nil && len(response.Skus) != 1 {
		apiErr = cutil.NewAPIError(http.StatusNotFound, "Could not find SKU with the specified ID", nil)
	}
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to retrieve SKU before update")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	apiReq.SkuID = skuID

	updatedSKU := &corev1.Sku{}
	if apiReq.Components == nil {
		updatedSKU = apiReq.ApplyMetadataToProto(response.Skus[0])
		logger.Info().Str("skuID", skuID).Str("siteID", siteID).Msg("updating SKU metadata via Core proxy")
		apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_UpdateSkuMetadata_FullMethodName, apiReq.ToMetadataProto(), nil, siteID)
		if apiErr != nil {
			logAPIError(logger, apiErr, "failed to update SKU metadata via Core proxy")
			return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
		}
	} else {
		updatedReq := apiReq.ToReplacementProto(response.Skus[0])
		logger.Info().Str("skuID", skuID).Str("siteID", siteID).Msg("updating SKU via Core proxy")
		apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_ReplaceSku_FullMethodName, updatedReq, updatedSKU, siteID)
		if apiErr != nil {
			logAPIError(logger, apiErr, "failed to update SKU via Core proxy")
			return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
		}
	}

	if updatedSKU == nil || updatedSKU.Id == "" {
		err = errors.New("cannot persist an empty Core SKU")
	} else {
		projected := &cdbm.SKU{}
		projected.FromProto(updatedSKU, savedSKU.SiteID)

		err = cdb.WithTx(ctx, ush.dbSession, func(tx *cdb.Tx) error {
			err := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(projected.ID), nil)
			if err != nil {
				return err
			}

			existing, err := skuDAO.Get(ctx, tx, projected.ID)
			if err != nil {
				return err
			}
			if existing.SiteID != projected.SiteID {
				return fmt.Errorf("SKU %q already exists for Site %q", projected.ID, existing.SiteID)
			}

			components := projected.Components
			if existing.Components != nil && components == nil {
				// DAO update uses nil to mean "not supplied", so use an empty value
				// to clear stale components when Core reports none.
				components = &cdbm.SkuComponents{SkuComponents: &corev1.SkuComponents{}}
			}
			associatedMachineIDs := projected.AssociatedMachineIds
			if associatedMachineIDs == nil {
				// DAO update uses nil to mean "not supplied"; Core nil means there
				// are no associations in the authoritative projection.
				associatedMachineIDs = []string{}
			}
			createdMatches := projected.Created.IsZero() || existing.Created.Equal(projected.Created)
			if createdMatches && existing.Description == projected.Description &&
				existing.SchemaVersion == projected.SchemaVersion &&
				existing.Components.Equal(components) &&
				reflect.DeepEqual(existing.DeviceType, projected.DeviceType) &&
				reflect.DeepEqual(existing.AssociatedMachineIds, associatedMachineIDs) {
				return nil
			}

			var created *time.Time
			if !createdMatches {
				created = &projected.Created
			}
			_, err = skuDAO.Update(ctx, tx, cdbm.SkuUpdateInput{
				SkuID:                projected.ID,
				Created:              created,
				Description:          &projected.Description,
				SchemaVersion:        &projected.SchemaVersion,
				Components:           components,
				DeviceType:           projected.DeviceType,
				AssociatedMachineIds: associatedMachineIDs,
			})
			return err
		})
	}
	if err != nil {
		logger.Error().Err(err).Str("skuID", skuID).Str("siteID", siteID).
			Msg("SKU updated in Core but failed to update REST DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "SKU was updated in Core but failed to update REST DB", nil)
	}

	return c.JSON(http.StatusOK, model.NewAPISkuFromProto(updatedSKU, siteID))
}

// DeleteSkuHandler deletes one unused SKU from a Site's Core service.
type DeleteSkuHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewDeleteSkuHandler returns a new DeleteSkuHandler.
func NewDeleteSkuHandler(dbSession *cdb.Session, scp *sc.ClientPool) DeleteSkuHandler {
	return DeleteSkuHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle godoc
// @Summary Delete SKU
// @Description Delete an unused SKU.
// @Tags SKU
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "SKU ID"
// @Success 204
// @Router /v2/org/{org}/nico/sku/{id} [delete]
func (dsh DeleteSkuHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("SKU", "Delete", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	skuID := c.Param("id")
	if skuID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "SKU ID must be specified", nil)
	}

	cotel.SetAttribute(handlerSpan, attribute.String("sku_id", skuID))

	skuDAO := cdbm.NewSkuDAO(dsh.dbSession)
	savedSKU, err := skuDAO.Get(ctx, nil, skuID)
	if errors.Is(err, cdb.ErrDoesNotExist) {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find SKU with the specified ID", nil)
	}
	if err != nil {
		logger.Error().Err(err).Str("skuID", skuID).Msg("error retrieving SKU from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SKU, DB error", nil)
	}
	if len(savedSKU.AssociatedMachineIds) > 0 {
		logger.Warn().Str("skuID", skuID).Msg("SKU is associated with machines and cannot be deleted")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "SKU is associated with machines and cannot be deleted", nil)
	}

	stc, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx:       ctx,
		Logger:    logger,
		DBSession: dsh.dbSession,
		SCP:       dsh.scp,
		Org:       org,
		User:      dbUser,
		SiteID:    savedSKU.SiteID.String(),
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	logger.Info().Str("skuID", skuID).Str("siteID", siteID).Msg("deleting SKU via Core proxy")
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_DeleteSku_FullMethodName, &corev1.SkuIdList{Ids: []string{skuID}}, nil, siteID)
	if apiErr != nil {
		if apiErr.Code != http.StatusNotFound {
			logAPIError(logger, apiErr, "failed to delete SKU via Core proxy")
			return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
		}
		logger.Warn().Str("skuID", skuID).Str("siteID", siteID).Msg("SKU not found in Core, removing stale REST DB record")
	}
	err = skuDAO.Delete(ctx, nil, skuID)
	if err != nil {
		logger.Error().Err(err).Str("skuID", skuID).Msg("failed to delete SKU from REST DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to delete SKU from REST DB", nil)
	}

	logger.Info().Str("skuID", skuID).Str("siteID", siteID).Msg("finishing API handler")
	return c.NoContent(http.StatusNoContent)
}
