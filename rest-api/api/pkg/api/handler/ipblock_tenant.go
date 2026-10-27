// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	tclient "go.temporal.io/sdk/client"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	createTenantIPBlockMethod    = corev1.Forge_CreateSitePrefix_FullMethodName
	updateTenantIPBlockMethod    = corev1.Forge_UpdateSitePrefix_FullMethodName
	deleteTenantIPBlockMethod    = corev1.Forge_DeleteSitePrefix_FullMethodName
	findTenantIPBlockMethod      = corev1.Forge_FindSitePrefixesByIds_FullMethodName
	tenantIPBlockAcceptedMessage = "Core accepted tenant IP Block mutation; waiting for inventory"
)

func (h CreateIPBlockHandler) handleTenant(c echo.Context, ctx context.Context, logger zerolog.Logger, org string, user *cdbm.User) error {
	tenant, apiErr := common.IsTenant(ctx, logger, h.dbSession, org, user, nil)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	ctx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()
	request := model.APIIPBlockCreateRequest{}
	err := decodeTenantIPBlockRequest(c.Request().Body, &request)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid IP Block request", validation.Errors{"body": err})
	}
	err = request.ValidateTenant()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid tenant IP Block request", err)
	}
	site, err := getTenantIPBlockSite(ctx, logger, h.dbSession, nil, uuid.MustParse(request.SiteID), tenant.ID, true)
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to retrieve Site")
	}
	if site.Config == nil || !site.Config.TenantSitePrefix {
		return cutil.NewAPIErrorResponse(c, http.StatusPreconditionFailed, "Tenant-created IP Blocks are disabled for this Site", nil)
	}
	stc, apiErr := getIPBlockSiteClient(h.sc, site.ID)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	var block *cdbm.IPBlock
	var detail *cdbm.StatusDetail
	err = cdb.WithTx(ctx, h.dbSession, func(tx *cdb.Tx) error {
		lockedSite, err := getTenantIPBlockSite(ctx, logger, h.dbSession, tx, site.ID, tenant.ID, true)
		if err != nil {
			return err
		}
		id := uuid.New()
		err = tx.TryAcquireAdvisoryLock(ctx, cdbm.SitePrefixIPBlockLockID(site.ID, id), nil)
		if err != nil {
			return err
		}
		if lockedSite.Config == nil || !lockedSite.Config.TenantSitePrefix {
			return cutil.NewAPIError(http.StatusPreconditionFailed, "Tenant-created IP Blocks are disabled for this Site", nil)
		}
		block, err = cdbm.NewIPBlockDAO(h.dbSession).Create(ctx, tx, cdbm.IPBlockCreateInput{IPBlockID: &id, SitePrefixID: &id, Managed: cutil.GetPtr(false), TenantID: &tenant.ID,
			Name: request.Name, Description: request.Description, SiteID: site.ID, InfrastructureProviderID: site.InfrastructureProviderID,
			Prefix: request.Prefix, PrefixLength: request.PrefixLength, ProtocolVersion: cdbm.IPBlockProtocolVersionV4,
			RoutingType: cdbm.IPBlockRoutingTypeDatacenterOnly, Status: cdbm.IPBlockStatusProvisioning, CreatedBy: &user.ID})
		if err != nil {
			return err
		}
		request.SitePrefixID, request.TenantOrg = id, org
		apiErr := common.ExecuteCoreGRPC(ctx, stc, createTenantIPBlockMethod, request.ToSitePrefixCreationRequest(), nil, "")
		if apiErr != nil {
			return tenantIPBlockCoreError(logger, apiErr)
		}
		detail, err = cdbm.NewStatusDetailDAO(h.dbSession).Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: block.ID.String(), Status: block.Status, Message: cutil.GetPtr(tenantIPBlockAcceptedMessage)})
		return err
	})
	if err != nil {
		return handleTenantIPBlockTxError(c, ctx, logger, err)
	}
	logger.Info().Msg("finishing API handler")
	c.Response().Header().Set(echo.HeaderLocation, strings.TrimRight(c.Request().URL.Path, "/")+"/"+block.ID.String())
	return c.JSON(http.StatusCreated, model.NewAPIIPBlock(block, []cdbm.StatusDetail{*detail}, nil))
}

func (h UpdateIPBlockHandler) handleTenant(c echo.Context, ctx context.Context, logger zerolog.Logger, org string, user *cdbm.User) error {
	tenant, apiErr := common.IsTenant(ctx, logger, h.dbSession, org, user, nil)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	ctx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()
	request := model.APIIPBlockUpdateRequest{}
	err := decodeTenantIPBlockRequest(c.Request().Body, &request)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid IP Block request", validation.Errors{"body": err})
	}
	err = request.ValidateTenant()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid tenant IP Block request", err)
	}
	block, apiErr := getTenantIPBlock(ctx, logger, h.dbSession, c.Param("id"), tenant.ID)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	site, err := getTenantIPBlockSite(ctx, logger, h.dbSession, nil, block.SiteID, tenant.ID, true)
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to retrieve Site")
	}
	stc, apiErr := getIPBlockSiteClient(h.sc, site.ID)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	var details []cdbm.StatusDetail
	err = cdb.WithTx(ctx, h.dbSession, func(tx *cdb.Tx) error {
		_, err := getTenantIPBlockSite(ctx, logger, h.dbSession, tx, site.ID, tenant.ID, true)
		if err != nil {
			return err
		}
		block, err = lockTenantIPBlock(ctx, h.dbSession, tx, block, tenant.ID)
		if err != nil {
			return err
		}
		if block.Status == cdbm.IPBlockStatusDeleting {
			return cutil.NewAPIError(http.StatusConflict, "IP Block is being deleted", nil)
		}
		// Preserve Core labels and fence a metadata update against concurrent writers.
		prefixes := &corev1.SitePrefixList{}
		apiErr := common.ExecuteCoreGRPC(ctx, stc, findTenantIPBlockMethod, block.ToSitePrefixesByIdsRequest(), prefixes, "")
		if apiErr != nil {
			return tenantIPBlockCoreError(logger, apiErr)
		}
		if len(prefixes.SitePrefixes) != 1 {
			return cutil.NewAPIError(http.StatusNotFound, "Core SitePrefix not found", nil)
		}
		current := prefixes.SitePrefixes[0]
		if current.GetId().GetValue() != block.SitePrefixID.String() || current.GetConfig().GetTenantOrganizationId() != org || current.GetMetadata() == nil || current.Version == "" || current.GetConfig().GetPrefix() != fmt.Sprintf("%s/%d", block.Prefix, block.PrefixLength) {
			return cutil.NewAPIError(http.StatusBadGateway, "Core returned an inconsistent SitePrefix", nil)
		}
		request.SitePrefixID, request.TenantOrg = *block.SitePrefixID, org
		coreRequest := request.ToSitePrefixUpdateRequest(current)
		apiErr = common.ExecuteCoreGRPC(ctx, stc, updateTenantIPBlockMethod, coreRequest, nil, "")
		if apiErr != nil {
			return tenantIPBlockCoreError(logger, apiErr)
		}
		description := &coreRequest.Metadata.Description
		if request.Description == nil && block.Description == nil && coreRequest.Metadata.Description == "" {
			// Preserve REST's absent description when Core has the same empty value.
			description = nil
		}
		block, err = cdbm.NewIPBlockDAO(h.dbSession).Update(ctx, tx, cdbm.IPBlockUpdateInput{IPBlockID: block.ID, Name: &coreRequest.Metadata.Name, Description: description})
		if err != nil {
			return err
		}
		details, err = cdbm.NewStatusDetailDAO(h.dbSession).GetRecentByEntityIDs(ctx, tx, []string{block.ID.String()}, common.RECENT_STATUS_DETAIL_COUNT)
		return err
	})
	if err != nil {
		return handleTenantIPBlockTxError(c, ctx, logger, err)
	}
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIIPBlock(block, details, nil))
}

func (h DeleteIPBlockHandler) handleTenant(c echo.Context, ctx context.Context, logger zerolog.Logger, org string, user *cdbm.User) error {
	tenant, apiErr := common.IsTenant(ctx, logger, h.dbSession, org, user, nil)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	ctx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()
	block, apiErr := getTenantIPBlock(ctx, logger, h.dbSession, c.Param("id"), tenant.ID)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	// Ownership still authorizes retirement after the last Allocation removes TenantSite.
	site, err := getTenantIPBlockSite(ctx, logger, h.dbSession, nil, block.SiteID, tenant.ID, false)
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to retrieve Site")
	}
	stc, apiErr := getIPBlockSiteClient(h.sc, site.ID)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	err = cdb.WithTx(ctx, h.dbSession, func(tx *cdb.Tx) error {
		_, err := getTenantIPBlockSite(ctx, logger, h.dbSession, tx, site.ID, tenant.ID, false)
		if err != nil {
			return err
		}
		block, err = lockTenantIPBlock(ctx, h.dbSession, tx, block, tenant.ID)
		if err != nil {
			return err
		}
		alreadyDeleting := block.Status == cdbm.IPBlockStatusDeleting
		if !alreadyDeleting {
			block, err = cdbm.NewIPBlockDAO(h.dbSession).Update(ctx, tx, cdbm.IPBlockUpdateInput{IPBlockID: block.ID, Status: cutil.GetPtr(cdbm.IPBlockStatusDeleting)})
			if err != nil {
				return err
			}
		}
		// Re-send retirement for a restored block or a previous unknown result.
		apiErr := common.ExecuteCoreGRPC(ctx, stc, deleteTenantIPBlockMethod, block.ToDeletionRequestProto(org), nil, "")
		statusMessage := tenantIPBlockAcceptedMessage
		if apiErr != nil && apiErr.Code == http.StatusNotFound {
			statusMessage = "Core reported SitePrefix absent; waiting for inventory"
			apiErr = nil
		}
		if apiErr != nil {
			return tenantIPBlockCoreError(logger, apiErr)
		}
		if !alreadyDeleting {
			_, err = cdbm.NewStatusDetailDAO(h.dbSession).Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: block.ID.String(), Status: block.Status, Message: &statusMessage})
		}
		return err
	})
	if err != nil {
		return handleTenantIPBlockTxError(c, ctx, logger, err)
	}
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusAccepted, model.NewAPIDeletionAcceptedResponse())
}

func decodeTenantIPBlockRequest(body io.Reader, target any) error {
	decoder := json.NewDecoder(body)
	decoder.DisallowUnknownFields()
	err := decoder.Decode(target)
	if err != nil {
		return err
	}
	var extra any
	err = decoder.Decode(&extra)
	if errors.Is(err, io.EOF) {
		return nil
	}
	if err == nil {
		return errors.New("multiple request objects")
	}
	return err
}

func getTenantIPBlock(ctx context.Context, logger zerolog.Logger, session *cdb.Session, idString string, tenantID uuid.UUID) (*cdbm.IPBlock, *cutil.APIError) {
	id, err := uuid.Parse(idString)
	if err != nil {
		return nil, cutil.NewAPIError(http.StatusBadRequest, "Invalid IP Block ID", nil)
	}
	block, err := cdbm.NewIPBlockDAO(session).GetOne(ctx, nil, id, cdbm.IPBlockFilterInput{TenantIDs: []uuid.UUID{tenantID}, Managed: cutil.GetPtr(false), CoreLinkedOnly: true}, nil)
	if errors.Is(err, cdb.ErrDoesNotExist) {
		return nil, cutil.NewAPIError(http.StatusNotFound, "IP Block not found", nil)
	}
	if err != nil {
		logger.Error().Err(err).Msg("failed to find tenant IP Block")
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve IP Block", nil)
	}
	return block, nil
}

// getTenantIPBlockSite rechecks admission under the lifecycle lock when tx is set.
// Association and Registered status are admission checks, not long-lived row locks.
func getTenantIPBlockSite(ctx context.Context, logger zerolog.Logger, session *cdb.Session, tx *cdb.Tx, siteID, tenantID uuid.UUID, requireAssociation bool) (*cdbm.Site, error) {
	var site *cdbm.Site
	var err error
	if tx != nil {
		site, err = cdbm.NewSiteDAO(session).GetByIDForTenantIPBlockMutation(ctx, tx, siteID)
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return nil, cutil.NewAPIError(http.StatusBadRequest, "Site is unavailable to this Tenant", nil)
		}
		if err != nil {
			return nil, err
		}
		if site.Status != cdbm.SiteStatusRegistered {
			return nil, cutil.NewAPIError(http.StatusBadRequest, "Site is not Registered", nil)
		}
	}
	if requireAssociation {
		_, err = cdbm.NewTenantSiteDAO(session).GetByTenantIDAndSiteID(ctx, tx, tenantID, siteID, nil)
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return nil, cutil.NewAPIError(http.StatusBadRequest, "Site is unavailable to this Tenant", nil)
		}
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Tenant Site association")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to validate Site access", nil)
		}
	}
	if tx == nil {
		site, err = common.GetSiteFromIDString(ctx, nil, siteID.String(), session)
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return nil, cutil.NewAPIError(http.StatusBadRequest, "Site is unavailable to this Tenant", nil)
		}
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve tenant IP Block Site")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Site", nil)
		}
		if site.Status != cdbm.SiteStatusRegistered {
			return nil, cutil.NewAPIError(http.StatusBadRequest, "Site is not Registered", nil)
		}
	}
	return site, nil
}

func getIPBlockSiteClient(pool *sc.ClientPool, siteID uuid.UUID) (tclient.Client, *cutil.APIError) {
	if pool == nil {
		return nil, cutil.NewAPIError(http.StatusServiceUnavailable, "Site client unavailable", nil)
	}
	client, err := pool.GetClientByID(siteID)
	if err != nil {
		return nil, cutil.NewAPIError(http.StatusServiceUnavailable, "Site client unavailable", nil)
	}
	return client, nil
}

func lockTenantIPBlock(ctx context.Context, session *cdb.Session, tx *cdb.Tx, block *cdbm.IPBlock, tenantID uuid.UUID) (*cdbm.IPBlock, error) {
	err := tx.TryAcquireAdvisoryLock(ctx, cdbm.SitePrefixIPBlockLockID(block.SiteID, *block.SitePrefixID), nil)
	if err != nil {
		return nil, err
	}
	block, err = cdbm.NewIPBlockDAO(session).GetByIDForUpdate(ctx, tx, block.ID)
	if err != nil {
		return nil, err
	}
	if block.Managed || block.TenantID == nil || *block.TenantID != tenantID || block.SitePrefixID == nil {
		return nil, cutil.NewAPIError(http.StatusNotFound, "IP Block not found", nil)
	}
	return block, nil
}

func tenantIPBlockCoreError(logger zerolog.Logger, apiErr *cutil.APIError) *cutil.APIError {
	// Core errors can contain provider configuration or other tenants' aggregate usage.
	logAPIError(logger, apiErr, "tenant IP Block Core request failed")
	return cutil.NewAPIError(apiErr.Code, "Core IP Block request failed: "+http.StatusText(apiErr.Code), nil)
}

func handleTenantIPBlockTxError(c echo.Context, ctx context.Context, logger zerolog.Logger, err error) error {
	if errors.Is(ctx.Err(), context.DeadlineExceeded) {
		return cutil.NewAPIErrorResponse(c, http.StatusGatewayTimeout, "Tenant IP Block request timed out", nil)
	}
	if errors.Is(err, cdb.ErrXactAdvisoryLockFailed) {
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Site or IP Block is being modified; retry the request", nil)
	}
	return common.HandleTxError(c, logger, err, "Failed to mutate tenant IP Block")
}
