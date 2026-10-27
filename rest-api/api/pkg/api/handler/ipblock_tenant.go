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
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	createTenantIPBlockMethod = corev1.Forge_CreateSitePrefix_FullMethodName
	updateTenantIPBlockMethod = corev1.Forge_UpdateSitePrefix_FullMethodName
	deleteTenantIPBlockMethod = corev1.Forge_DeleteSitePrefix_FullMethodName
	findTenantIPBlockMethod   = corev1.Forge_FindSitePrefixesByIds_FullMethodName
)

// tenantIPBlockHandler handles tenant mutations on the existing IP Block routes.
// Provider mutations retain their allocation and cloud-IPAM behavior.
type tenantIPBlockHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

func (h tenantIPBlockHandler) handle(c echo.Context) error {
	org, user, ctx, logger, span := common.SetupHandler("IPBlock", "TenantMutation", c)
	if span != nil {
		defer span.End()
	}
	tenant, apiErr := common.IsTenant(ctx, logger, h.dbSession, org, user, nil)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	ctx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()
	method := c.Request().Method
	createRequest := model.APIIPBlockCreateRequest{}
	updateRequest := model.APIIPBlockUpdateRequest{}
	if method != http.MethodDelete {
		var target any = &createRequest
		if method == http.MethodPatch {
			target = &updateRequest
		}
		decoder := json.NewDecoder(c.Request().Body)
		decoder.DisallowUnknownFields()
		err := decoder.Decode(target)
		if err == nil {
			var extra any
			err = decoder.Decode(&extra)
			if errors.Is(err, io.EOF) {
				err = nil
			} else if err == nil {
				err = errors.New("multiple request objects")
			}
		}
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid IP Block request", validation.Errors{"body": err})
		}
		if method == http.MethodPost {
			err = createRequest.ValidateTenant()
		} else {
			err = updateRequest.ValidateTenant()
		}
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid tenant IP Block request", err)
		}
	}
	dao := cdbm.NewIPBlockDAO(h.dbSession)
	var block *cdbm.IPBlock
	siteID := createRequest.SiteID
	if method != http.MethodPost {
		id, err := uuid.Parse(c.Param("id"))
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid IP Block ID", nil)
		}
		block, err = dao.GetOne(ctx, nil, id, cdbm.IPBlockFilterInput{TenantIDs: []uuid.UUID{tenant.ID}, Managed: cutil.GetPtr(false), CoreLinkedOnly: true}, nil)
		if err != nil {
			if errors.Is(err, cdb.ErrDoesNotExist) {
				return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "IP Block not found", nil)
			}
			logger.Error().Err(err).Msg("failed to find tenant IP Block")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve IP Block", nil)
		}
		siteID = block.SiteID.String()
	}
	// Creation and metadata updates require an active association. Ownership still authorizes
	// retiring an existing root after the last Allocation removes TenantSite.
	if method != http.MethodDelete {
		_, err := cdbm.NewTenantSiteDAO(h.dbSession).GetByTenantIDAndSiteID(ctx, nil, tenant.ID, uuid.MustParse(siteID), nil)
		if err != nil {
			if errors.Is(err, cdb.ErrDoesNotExist) {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site is unavailable to this Tenant", nil)
			}
			logger.Error().Err(err).Msg("failed to retrieve Tenant Site association")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to validate Site access", nil)
		}
	}
	site, err := common.GetSiteFromIDString(ctx, nil, siteID, h.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site is unavailable to this Tenant", nil)
		}
		logger.Error().Err(err).Msg("failed to retrieve tenant IP Block Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site", nil)
	}
	if site.Status != cdbm.SiteStatusRegistered {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site is not Registered", nil)
	}
	if method == http.MethodPost && (site.Config == nil || !site.Config.TenantSitePrefix) {
		return cutil.NewAPIErrorResponse(c, http.StatusPreconditionFailed, "Tenant-created IP Blocks are disabled for this Site", nil)
	}
	if h.scp == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusServiceUnavailable, "Site client unavailable", nil)
	}
	stc, err := h.scp.GetClientByID(site.ID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusServiceUnavailable, "Site client unavailable", nil)
	}
	var details []cdbm.StatusDetail
	deletionStarted := false
	statusMessage := "Core accepted tenant IP Block mutation; waiting for inventory"
	err = cdb.WithTx(ctx, h.dbSession, func(tx *cdb.Tx) error {
		lockedSite, err := cdbm.NewSiteDAO(h.dbSession).GetByIDForTenantIPBlockMutation(ctx, tx, site.ID)
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIError(http.StatusBadRequest, "Site is unavailable to this Tenant", nil)
		}
		if err != nil {
			return err
		}
		if lockedSite.Status != cdbm.SiteStatusRegistered {
			return cutil.NewAPIError(http.StatusBadRequest, "Site is not Registered", nil)
		}
		// Association and Registered status are admission checks. Once admitted,
		// a mutation can finish even if the association or status later changes.
		if method != http.MethodDelete {
			_, err = cdbm.NewTenantSiteDAO(h.dbSession).GetByTenantIDAndSiteID(ctx, tx, tenant.ID, site.ID, nil)
			if errors.Is(err, cdb.ErrDoesNotExist) {
				return cutil.NewAPIError(http.StatusBadRequest, "Site is unavailable to this Tenant", nil)
			}
			if err != nil {
				return err
			}
		}
		prefixID := uuid.New()
		if method != http.MethodPost {
			prefixID = *block.SitePrefixID
		}
		err = tx.TryAcquireAdvisoryLock(ctx, cdbm.SitePrefixIPBlockLockID(site.ID, prefixID), nil)
		if err != nil {
			return err
		}
		if method == http.MethodPost {
			if lockedSite.Config == nil || !lockedSite.Config.TenantSitePrefix {
				return cutil.NewAPIError(http.StatusPreconditionFailed, "Tenant-created IP Blocks are disabled for this Site", nil)
			}
			id := prefixID
			block, err = dao.Create(ctx, tx, cdbm.IPBlockCreateInput{IPBlockID: &id, SitePrefixID: &id, Managed: cutil.GetPtr(false), TenantID: &tenant.ID,
				Name: createRequest.Name, Description: createRequest.Description, SiteID: site.ID, InfrastructureProviderID: site.InfrastructureProviderID,
				Prefix: createRequest.Prefix, PrefixLength: createRequest.PrefixLength, ProtocolVersion: cdbm.IPBlockProtocolVersionV4,
				RoutingType: cdbm.IPBlockRoutingTypeDatacenterOnly, Status: cdbm.IPBlockStatusProvisioning, CreatedBy: &user.ID})
			if err != nil {
				return err
			}
			req := createRequest.ToSitePrefixCreationRequest(id, org)
			apiErr = common.ExecuteCoreGRPC(ctx, stc, createTenantIPBlockMethod, req, nil, "")
		} else {
			block, err = dao.GetByIDForUpdate(ctx, tx, block.ID)
			if err != nil {
				return err
			}
			if block.Managed || block.TenantID == nil || *block.TenantID != tenant.ID || block.SitePrefixID == nil {
				return cutil.NewAPIError(http.StatusNotFound, "IP Block not found", nil)
			}
			id := &corev1.SitePrefixId{Value: block.SitePrefixID.String()}
			if method == http.MethodDelete {
				alreadyDeleting := block.Status == cdbm.IPBlockStatusDeleting
				if !alreadyDeleting {
					block, err = dao.Update(ctx, tx, cdbm.IPBlockUpdateInput{IPBlockID: block.ID, Status: cutil.GetPtr(cdbm.IPBlockStatusDeleting)})
					if err != nil {
						return err
					}
					deletionStarted = true
				}
				// Re-send retirement for a restored block or a previous unknown result.
				apiErr = common.ExecuteCoreGRPC(ctx, stc, deleteTenantIPBlockMethod, &corev1.SitePrefixDeletionRequest{Id: id, TenantOrganizationId: org}, nil, "")
				if apiErr != nil && apiErr.Code == http.StatusNotFound {
					statusMessage = "Core reported SitePrefix absent; waiting for inventory"
					apiErr = nil
				}
			} else {
				if block.Status == cdbm.IPBlockStatusDeleting {
					return cutil.NewAPIError(http.StatusConflict, "IP Block is being deleted", nil)
				}
				// Preserve Core labels and fence a metadata update against concurrent writers.
				prefixes := &corev1.SitePrefixList{}
				apiErr = common.ExecuteCoreGRPC(ctx, stc, findTenantIPBlockMethod, &corev1.SitePrefixesByIdsRequest{SitePrefixIds: []*corev1.SitePrefixId{id}}, prefixes, "")
				if apiErr != nil {
					logAPIError(logger, apiErr, "tenant IP Block Core lookup failed")
					return cutil.NewAPIError(apiErr.Code, "Core IP Block request failed: "+http.StatusText(apiErr.Code), nil)
				}
				if len(prefixes.SitePrefixes) != 1 {
					return cutil.NewAPIError(http.StatusNotFound, "Core SitePrefix not found", nil)
				}
				current := prefixes.SitePrefixes[0]
				if current.GetId().GetValue() != id.Value || current.GetConfig().GetTenantOrganizationId() != org || current.GetMetadata() == nil || current.Version == "" || current.GetConfig().GetPrefix() != fmt.Sprintf("%s/%d", block.Prefix, block.PrefixLength) {
					return cutil.NewAPIError(http.StatusBadGateway, "Core returned an inconsistent SitePrefix", nil)
				}
				metadata := proto.Clone(current.Metadata).(*corev1.Metadata)
				if updateRequest.Name != nil {
					metadata.Name = *updateRequest.Name
				}
				if updateRequest.Description != nil {
					metadata.Description = *updateRequest.Description
				}
				apiErr = common.ExecuteCoreGRPC(ctx, stc, updateTenantIPBlockMethod, &corev1.SitePrefixUpdateRequest{Id: id, TenantOrganizationId: org, Metadata: metadata, IfVersionMatch: &current.Version}, nil, "")
				if apiErr == nil {
					description := &metadata.Description
					if updateRequest.Description == nil && block.Description == nil && metadata.Description == "" {
						// Preserve REST's absent description when Core has the same empty value.
						description = nil
					}
					block, err = dao.Update(ctx, tx, cdbm.IPBlockUpdateInput{IPBlockID: block.ID, Name: &metadata.Name, Description: description})
					if err != nil {
						return err
					}
				}
			}
		}
		if apiErr != nil {
			// Core errors can contain provider configuration or other tenants' aggregate usage.
			logAPIError(logger, apiErr, "tenant IP Block Core mutation failed")
			return cutil.NewAPIError(apiErr.Code, "Core IP Block request failed: "+http.StatusText(apiErr.Code), nil)
		}
		sdDAO := cdbm.NewStatusDetailDAO(h.dbSession)
		if method == http.MethodPost || deletionStarted {
			detail, err := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: block.ID.String(), Status: block.Status, Message: &statusMessage})
			if err != nil {
				return err
			}
			details = []cdbm.StatusDetail{*detail}
		} else if method == http.MethodPatch {
			details, err = sdDAO.GetRecentByEntityIDs(ctx, tx, []string{block.ID.String()}, common.RECENT_STATUS_DETAIL_COUNT)
			if err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		if errors.Is(ctx.Err(), context.DeadlineExceeded) {
			return cutil.NewAPIErrorResponse(c, http.StatusGatewayTimeout, "Tenant IP Block request timed out", nil)
		}
		if errors.Is(err, cdb.ErrXactAdvisoryLockFailed) {
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Site or IP Block is being modified; retry the request", nil)
		}
		return common.HandleTxError(c, logger, err, "Failed to mutate tenant IP Block")
	}
	if method == http.MethodDelete {
		return c.JSON(http.StatusAccepted, model.NewAPIDeletionAcceptedResponse())
	}
	code := http.StatusOK
	if method == http.MethodPost {
		code = http.StatusCreated
		c.Response().Header().Set(echo.HeaderLocation, strings.TrimRight(c.Request().URL.Path, "/")+"/"+block.ID.String())
	}
	return c.JSON(code, model.NewAPIIPBlock(block, details, nil))
}
