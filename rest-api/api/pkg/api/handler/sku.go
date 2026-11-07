// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"reflect"
	"slices"
	"time"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"go.opentelemetry.io/otel/attribute"
	tclient "go.temporal.io/sdk/client"
	"google.golang.org/protobuf/types/known/emptypb"
	"google.golang.org/protobuf/types/known/structpb"

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
	site, err := siteDAO.GetByID(ctx, nil, sku.SiteID, nil, false, false)
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

	apiErr = checkSkuMutationAllowed(ctx, csh.dbSession, apiReq.ID, uuid.MustParse(siteID), true)
	if apiErr != nil {
		if apiErr.Code >= http.StatusInternalServerError {
			logger.Error().Err(apiErr.Diagnosis()).Str("skuID", apiReq.ID).Str("siteID", siteID).Msg("failed to check SKU mutation state")
		}
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
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
			_, err := cdbm.NewSiteDAO(csh.dbSession).GetForShare(ctx, tx, projected.SiteID)
			if err != nil {
				return err
			}
			err = tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(projected.ID), nil)
			if err != nil {
				return err
			}

			existing, lookupErr := skuDAO.Get(ctx, tx, projected.ID)
			if lookupErr != nil && !errors.Is(lookupErr, cdb.ErrDoesNotExist) {
				return lookupErr
			}
			if lookupErr == nil && existing.Deletion != nil {
				return cutil.NewAPIError(http.StatusConflict, "SKU deletion is pending", nil)
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
		var conflict *cutil.APIError
		if errors.As(err, &conflict) {
			return conflict.Send(c)
		}
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

	apiErr = checkSkuMutationAllowed(ctx, ush.dbSession, skuID, savedSKU.SiteID, false)
	if apiErr != nil {
		if apiErr.Code >= http.StatusInternalServerError {
			logger.Error().Err(apiErr.Diagnosis()).Str("skuID", skuID).Str("siteID", siteID).Msg("failed to check SKU mutation state")
		}
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	// All Core calls share one wait budget. Keep DB work on the request context
	// so exhausting that budget after a completed update does not prevent saving it.
	rpcCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	apiReq.SkuID = skuID

	coreSKU := &corev1.Sku{}
	useComponentReplacement := false
	if apiReq.Components != nil {
		if rpcCtx.Err() != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusGatewayTimeout, "SKU update timed out before Core could apply it", nil)
		}
		var buildInfo structpb.Struct
		apiErr = common.ExecuteCoreGRPC(rpcCtx, stc, corev1.Forge_Version_FullMethodName, &corev1.VersionRequest{}, &buildInfo, siteID)
		if apiErr != nil {
			// Discovery is read-only, so a failed probe can still use
			// `FindSkusByIds` and `ReplaceSku` while the request is live.
			logger.Warn().Err(apiErr.Diagnosis()).Msg("failed to check SKU component replacement support")
		} else {
			// A Site Agent that knows this capability's name also has the RPC
			// in its compiled registry. Older agents emit a number, which keeps
			// full replacement. Decoding into `BuildInfo` loses that distinction.
			capabilities := buildInfo.GetFields()["capabilities"].GetListValue().GetValues()
			useComponentReplacement = slices.ContainsFunc(capabilities, func(capability *structpb.Value) bool {
				return capability.GetStringValue() == corev1.BuildCapability_BUILD_CAPABILITY_REPLACE_SKU_COMPONENTS.String()
			})
		}
	}

	if rpcCtx.Err() != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusGatewayTimeout, "SKU update timed out before Core could apply it", nil)
	}
	var method string
	if useComponentReplacement {
		method = corev1.Forge_ReplaceSkuComponents_FullMethodName
		logger.Info().Str("skuID", skuID).Str("siteID", siteID).Msg("replacing SKU components via Core proxy")
		apiErr = common.ExecuteCoreGRPC(rpcCtx, stc, method, apiReq.ToComponentReplacementProto(), coreSKU, siteID)
		if apiErr != nil && skuComponentReplacementRejected(apiErr) {
			// During an upgrade the probe and update can reach different Agent or
			// Core versions. Only a rejection before dispatch permits `ReplaceSku`;
			// any other failure may follow a completed update.
			logger.Warn().Err(apiErr.Diagnosis()).Msg("SKU component replacement rejected before dispatch; using full replacement")
			useComponentReplacement = false
		}
	}
	if !useComponentReplacement {
		if rpcCtx.Err() != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusGatewayTimeout, "SKU update timed out before Core could apply it", nil)
		}
		// `UpdateSkuMetadata` returns no record, so metadata updates need this
		// snapshot. Full replacement also needs it to fill in omitted metadata.
		var response corev1.SkuList
		apiErr = common.ExecuteCoreGRPC(
			rpcCtx,
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
		if rpcCtx.Err() != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusGatewayTimeout, "SKU update timed out before Core could apply it", nil)
		}

		if apiReq.Components == nil {
			coreSKU = response.Skus[0]
			method = corev1.Forge_UpdateSkuMetadata_FullMethodName
			logger.Info().Str("skuID", skuID).Str("siteID", siteID).Msg("updating SKU metadata via Core proxy")
			apiErr = common.ExecuteCoreGRPC(rpcCtx, stc, method, apiReq.ToMetadataProto(), nil, siteID)
		} else {
			method = corev1.Forge_ReplaceSku_FullMethodName
			logger.Info().Str("skuID", skuID).Str("siteID", siteID).Msg("replacing full SKU via Core proxy")
			apiErr = common.ExecuteCoreGRPC(rpcCtx, stc, method, apiReq.ToReplacementProto(response.Skus[0]), coreSKU, siteID)
		}
	}
	if apiErr != nil {
		logger.Error().Str("method", method).Err(apiErr.Diagnosis()).Msg("failed to update SKU via Core proxy")
		if method == corev1.Forge_ReplaceSkuComponents_FullMethodName && apiErr.Code == http.StatusNotFound {
			// Keep the response callers received when the preceding Find failed.
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find SKU with the specified ID", nil)
		}
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	if coreSKU == nil || coreSKU.Id == "" {
		err = errors.New("cannot persist an empty Core SKU")
	} else {
		projected := &cdbm.SKU{}
		projected.FromProto(coreSKU, savedSKU.SiteID)
		// Component replacement uses metadata from Core's response, preserving
		// the saved device type when Core omits it.
		description := &projected.Description
		deviceType := projected.DeviceType
		if apiReq.Components == nil {
			// Another writer may have changed omitted metadata since the Core read.
			// Save only supplied metadata; nonmetadata still refreshes from Core.
			description = apiReq.Description
			deviceType = apiReq.DeviceType
		}

		err = cdb.WithTx(ctx, ush.dbSession, func(tx *cdb.Tx) error {
			_, err := cdbm.NewSiteDAO(ush.dbSession).GetForShare(ctx, tx, projected.SiteID)
			if err != nil {
				return err
			}
			err = tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(projected.ID), nil)
			if err != nil {
				return err
			}

			existing, err := skuDAO.Get(ctx, tx, projected.ID)
			if err != nil {
				return err
			}
			if existing.Deletion != nil {
				return cutil.NewAPIError(http.StatusConflict, "SKU deletion is pending", nil)
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
			descriptionMatches := description == nil || existing.Description == *description
			deviceTypeMatches := deviceType == nil || reflect.DeepEqual(existing.DeviceType, deviceType)
			if createdMatches && descriptionMatches && deviceTypeMatches &&
				existing.SchemaVersion == projected.SchemaVersion &&
				existing.Components.Equal(components) &&
				reflect.DeepEqual(existing.AssociatedMachineIds, associatedMachineIDs) {
				savedSKU = existing
				return nil
			}

			var created *time.Time
			if !createdMatches {
				created = &projected.Created
			}
			savedSKU, err = skuDAO.Update(ctx, tx, cdbm.SkuUpdateInput{
				SkuID:                projected.ID,
				Created:              created,
				Description:          description,
				SchemaVersion:        &projected.SchemaVersion,
				Components:           components,
				DeviceType:           deviceType,
				AssociatedMachineIds: associatedMachineIDs,
			})
			return err
		})
	}
	if err != nil {
		var conflict *cutil.APIError
		if errors.As(err, &conflict) {
			return conflict.Send(c)
		}
		logger.Error().Err(err).Str("skuID", skuID).Str("siteID", siteID).
			Msg("SKU updated in Core but failed to update REST DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "SKU was updated in Core but failed to update REST DB", nil)
	}

	// Keep Core's Created nanoseconds and leave it unset when Core omits it.
	// REST can retain a device type that Core omitted, so use the saved value.
	apiResponse := model.NewAPISkuFromProto(coreSKU, siteID)
	apiResponse.DeviceType = savedSKU.DeviceType
	if apiReq.Components == nil {
		// `UpdateSkuMetadata` returns no SKU, so use the description we saved.
		apiResponse.Description = savedSKU.Description
	}
	return c.JSON(http.StatusOK, apiResponse)
}

const skuDeletionPersistenceTimeout = 5 * time.Second

// DeleteSkuHandler deletes one unused SKU from a Site's Core service.
type DeleteSkuHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteSkuHandler returns a new DeleteSkuHandler.
func NewDeleteSkuHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) DeleteSkuHandler {
	return DeleteSkuHandler{dbSession: dbSession, scp: scp, cfg: cfg}
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
	logger = logger.With().Str("skuID", skuID).Str("siteID", savedSKU.SiteID.String()).Logger()
	authInput := common.AuthorizeProviderSiteForCoreInput{
		Ctx:       ctx,
		Logger:    logger,
		DBSession: dsh.dbSession,
		SCP:       dsh.scp,
		Org:       org,
		User:      dbUser,
		SiteID:    savedSKU.SiteID.String(),
	}
	// Authorization precedes association and reference disclosure. Confirmed
	// local cleanup needs neither a Registered Site nor a Temporal connection.
	_, apiErr := common.AuthorizeProviderSite(authInput)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}
	canStart := savedSKU.Deletion == nil
	capturedWorkflowID := ""
	if savedSKU.Deletion != nil {
		capturedWorkflowID = savedSKU.Deletion.WorkflowID
	}
	var stc tclient.Client
	siteID := savedSKU.SiteID.String()
	if canStart {
		if len(savedSKU.AssociatedMachineIds) > 0 {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "SKU is associated with machines and cannot be deleted", nil)
		}
		if dsh.scp == nil {
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}
		// Resolve local prerequisites before recording an attempt. An unavailable
		// client cannot have dispatched Core and must not leave an unbound fence.
		stc, siteID, apiErr = common.AuthorizeProviderSiteForCore(authInput)
		if apiErr != nil {
			return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
		}
	}
	startNewAttempt := false
	err = cdb.WithTx(ctx, dsh.dbSession, func(tx *cdb.Tx) error {
		site, err := cdbm.NewSiteDAO(dsh.dbSession).GetForShare(ctx, tx, savedSKU.SiteID)
		if err != nil {
			return err
		}
		err = tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(skuID), nil)
		if err != nil {
			return err
		}
		current, err := skuDAO.GetForUpdate(ctx, tx, skuID)
		if err != nil {
			return err
		}
		if current.SiteID != savedSKU.SiteID {
			return cutil.NewAPIError(http.StatusConflict, "SKU Site changed during deletion", nil)
		}
		if current.Deletion != nil && !canStart && current.Deletion.WorkflowID != capturedWorkflowID {
			return cutil.NewAPIError(http.StatusConflict, "SKU deletion attempt changed; retry the request", nil)
		}
		savedSKU = current
		if current.Deletion != nil {
			return nil
		}
		if !canStart {
			return cutil.NewAPIError(http.StatusConflict, "SKU deletion attempt changed; retry the request", nil)
		}
		if len(current.AssociatedMachineIds) > 0 {
			return cutil.NewAPIError(http.StatusBadRequest, "SKU is associated with machines and cannot be deleted", nil)
		}
		referenced, err := skuDAO.HasExpectedMachineReferences(ctx, tx, skuID)
		if err != nil {
			return err
		}
		if referenced {
			return cutil.NewAPIError(http.StatusBadRequest, "SKU is referenced by Expected Machines and cannot be deleted", nil)
		}
		if dsh.cfg == nil || !dsh.cfg.GetSkuDeletionRecoveryEnabled() {
			return nil
		}
		if site.Status != cdbm.SiteStatusRegistered {
			return cutil.NewAPIError(http.StatusBadRequest, "Site must be Registered to delete SKU", nil)
		}
		deletion := &cdbm.SkuDeletion{WorkflowID: "core-grpc-DeleteSku-" + uuid.NewString()}
		applied, err := skuDAO.SetDeletion(ctx, tx, skuID, current.SiteID, nil, deletion)
		if err != nil {
			return err
		}
		if !applied {
			return errors.New("SKU deletion intent was not persisted")
		}
		current.Deletion = deletion
		startNewAttempt = true
		return nil
	})
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find SKU with the specified ID", nil)
		}
		if errors.Is(err, cdb.ErrXactAdvisoryLockFailed) {
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "SKU is being updated; retry the request", nil)
		}
		return common.HandleTxError(c, logger, err, "Failed to prepare SKU deletion, DB error")
	}
	if savedSKU.Deletion != nil && savedSKU.Deletion.Confirmed {
		applied := false
		applied, err = skuDAO.ResolveDeletion(ctx, skuID, savedSKU.SiteID, savedSKU.Deletion.WorkflowID, true)
		if err == nil && !applied {
			err = skuCleanupNotApplied(ctx, skuDAO, skuID)
		}
		if err != nil {
			return common.HandleTxError(c, logger, err, "Core deleted SKU but local cleanup failed")
		}
		return c.NoContent(http.StatusNoContent)
	}
	if startNewAttempt && ctx.Err() != nil {
		// No workflow invocation occurred. Release only this request's matching
		// intent, using a separate bound so cancellation cannot skip the DB cleanup.
		releaseCtx, cancelRelease := context.WithTimeout(context.WithoutCancel(ctx), skuDeletionPersistenceTimeout)
		defer cancelRelease()
		_, releaseErr := skuDAO.ResolveDeletion(releaseCtx, skuID, savedSKU.SiteID, savedSKU.Deletion.WorkflowID, false)
		if releaseErr != nil {
			return common.HandleTxError(c, logger, releaseErr, "Failed to release undispatched SKU deletion")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusGatewayTimeout, "SKU deletion timed out before Core dispatch", nil)
	}
	if savedSKU.Deletion != nil && !startNewAttempt {
		if dsh.scp == nil {
			return cutil.NewAPIErrorResponse(c, http.StatusServiceUnavailable, "Failed to retrieve client for pending SKU deletion", nil)
		}
		stc, err = dsh.scp.GetClientByID(savedSKU.SiteID)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusServiceUnavailable, "Failed to retrieve client for pending SKU deletion", nil)
		}
	}
	logger.Info().Str("skuID", skuID).Str("siteID", siteID).Msg("deleting SKU via Core proxy")
	if savedSKU.Deletion == nil {
		// Activation requires quiescing legacy mutations and enabling every API
		// writer before resuming them. Existing intents never enter this branch.
		apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_DeleteSku_FullMethodName, &corev1.SkuIdList{Ids: []string{skuID}}, nil, siteID)
		if apiErr != nil && apiErr.Code != http.StatusNotFound {
			logAPIError(logger, apiErr, "failed to delete SKU via Core proxy")
			return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
		}
		applied := false
		applied, err = skuDAO.DeleteIfUnchanged(ctx, nil, skuID, savedSKU.SiteID, savedSKU.Updated)
		if err == nil && !applied {
			err = skuCleanupNotApplied(ctx, skuDAO, skuID)
		}
	} else {
		deletion := savedSKU.Deletion
		runID := ""
		if deletion.RunID != nil {
			runID = *deletion.RunID
		}
		var observed bool
		// Core deletion is ID-only. This attempt fences cooperating REST writers
		// and local cleanup; it cannot fence a SKU recreated directly in Core.
		apiErr, observed = common.ExecuteCoreGRPCWithRunID(ctx, stc, corev1.Forge_DeleteSku_FullMethodName,
			&corev1.SkuIdList{Ids: []string{skuID}}, &emptypb.Empty{}, deletion.WorkflowID, runID, startNewAttempt,
			func(runID string) error {
				// The start was acknowledged even if the caller stopped waiting.
				bindCtx, cancelBind := context.WithTimeout(context.WithoutCancel(ctx), skuDeletionPersistenceTimeout)
				defer cancelBind()
				return skuDAO.BindDeletionRun(bindCtx, skuID, savedSKU.SiteID, deletion.WorkflowID, runID)
			})
		if observed {
			confirmed := apiErr == nil || apiErr.Code == http.StatusNotFound
			completionCtx, cancelCompletion := context.WithTimeout(context.WithoutCancel(ctx), skuDeletionPersistenceTimeout)
			applied, resolveErr := skuDAO.ResolveDeletion(completionCtx, skuID, savedSKU.SiteID, deletion.WorkflowID, confirmed)
			err = resolveErr
			if err == nil && !applied {
				current, lookupErr := skuDAO.Get(completionCtx, nil, skuID)
				switch {
				case errors.Is(lookupErr, cdb.ErrDoesNotExist):
				case lookupErr != nil:
					err = lookupErr
				case !confirmed && current.SiteID == savedSKU.SiteID && current.Deletion == nil:
					// Another observer already released this rejection. Keep the original
					// Core error instead of replacing it with a local cleanup conflict.
				default:
					err = cutil.NewAPIError(http.StatusConflict, "SKU changed while Core deletion was running; local record retained", nil)
				}
			}
			cancelCompletion()
		}
		if err == nil && apiErr != nil && (!observed || apiErr.Code != http.StatusNotFound) {
			logAPIError(logger, apiErr, "SKU deletion result did not confirm completion")
			return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
		}
	}
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to delete SKU from REST DB")
	}
	return c.NoContent(http.StatusNoContent)
}

// checkSkuMutationAllowed reloads the local fence before a Core create/update.
// Projection writes repeat this check because a delete can start during the RPC.
func checkSkuMutationAllowed(ctx context.Context, session *cdb.Session, skuID string, siteID uuid.UUID, allowMissing bool) *cutil.APIError {
	_, err := cdbm.NewSiteDAO(session).GetByID(ctx, nil, siteID, nil, false, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIError(http.StatusNotFound, "Could not find Site for the SKU", nil)
		}
		return cutil.NewAPIError(http.StatusInternalServerError, "Failed to check SKU Site", nil).WithCause(err)
	}
	sku, err := cdbm.NewSkuDAO(session).Get(ctx, nil, skuID)
	if errors.Is(err, cdb.ErrDoesNotExist) && allowMissing {
		return nil
	}
	if errors.Is(err, cdb.ErrDoesNotExist) {
		return cutil.NewAPIError(http.StatusNotFound, "Could not find SKU with the specified ID", nil)
	}
	if err != nil {
		return cutil.NewAPIError(http.StatusInternalServerError, "Failed to check SKU deletion state", nil).WithCause(err)
	}
	if sku.SiteID != siteID {
		return cutil.NewAPIError(http.StatusConflict, "SKU belongs to another Site", nil)
	}
	if sku.Deletion != nil {
		return cutil.NewAPIError(http.StatusConflict, "SKU deletion is pending", nil)
	}
	return nil
}

func skuCleanupNotApplied(ctx context.Context, dao cdbm.SkuDAO, skuID string) error {
	_, err := dao.Get(ctx, nil, skuID)
	if errors.Is(err, cdb.ErrDoesNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	return cutil.NewAPIError(http.StatusConflict, "SKU changed while Core deletion was running; local record retained", nil)
}
