// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"errors"
	"net/http"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"google.golang.org/protobuf/types/known/emptypb"
)

// expectedInventoryBulkBase holds the dependencies shared by full-Site
// Expected Inventory mutations.
type expectedInventoryBulkBase struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

func newExpectedInventoryBulkBase(dbSession *cdb.Session, scp *sc.ClientPool, _ *config.Config) expectedInventoryBulkBase {
	return expectedInventoryBulkBase{
		dbSession: dbSession,
		scp:       scp,
	}
}

func (b expectedInventoryBulkBase) resolveSite(ctx context.Context, logger zerolog.Logger, org string, dbUser *cdbm.User, siteID string, requireRegistered bool) (*cdbm.Site, *cutil.APIError) {
	site, err := common.GetSiteFromIDString(ctx, nil, siteID, b.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return nil, cutil.NewAPIError(http.StatusBadRequest, "Site specified in request does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Site due to DB error", nil)
	}

	infrastructureProvider, tenant, apiErr := common.IsProviderOrTenant(ctx, logger, b.dbSession, org, dbUser, false, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiErr != nil {
		return nil, apiErr
	}
	hasAccess, apiErr := ValidateProviderOrTenantSiteAccess(ctx, logger, b.dbSession, site, infrastructureProvider, tenant)
	if apiErr != nil {
		return nil, apiErr
	}
	if !hasAccess {
		return nil, cutil.NewAPIError(http.StatusForbidden, "Current org is not associated with the Site", nil)
	}
	if requireRegistered && site.Status != cdbm.SiteStatusRegistered {
		return nil, cutil.NewAPIError(http.StatusBadRequest, "Site is not in Registered state, cannot perform operation", nil)
	}
	return site, nil
}

func validateDeleteAllSiteID(c echo.Context) (string, *cutil.APIError) {
	siteID := c.QueryParam("siteId")
	if siteID == "" {
		return "", cutil.NewAPIError(http.StatusBadRequest, "siteId query parameter is required", nil)
	}
	if _, err := uuid.Parse(siteID); err != nil {
		return "", cutil.NewAPIError(http.StatusBadRequest, "Invalid siteId in query parameter", nil)
	}
	return siteID, nil
}

func (b expectedInventoryBulkBase) deleteAll(c echo.Context, resource, method string, deleteDB func(context.Context, *cdb.Tx, uuid.UUID) error) error {
	org, dbUser, ctx, logger, span := common.SetupHandler(resource, "DeleteAll", c)
	if span != nil {
		defer span.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	siteID, apiErr := validateDeleteAllSiteID(c)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}
	site, apiErr := b.resolveSite(ctx, logger, org, dbUser, siteID, false)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}
	logger = logger.With().Str("SiteID", site.ID.String()).Logger()
	stc, err := b.scp.GetClientByID(site.ID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}
	err = cdb.WithTx(ctx, b.dbSession, func(tx *cdb.Tx) error {
		derr := deleteDB(ctx, tx, site.ID)
		if derr != nil {
			logger.Error().Err(derr).Msg("error deleting Expected Inventory records from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Expected Inventory due to DB error", nil)
		}
		apiErr := common.ExecuteCoreGRPC(ctx, stc, method, &emptypb.Empty{}, nil, site.ID.String())
		if apiErr != nil {
			return apiErr
		}
		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to delete Expected Inventory due to DB transaction error")
	}
	return c.NoContent(http.StatusNoContent)
}
