// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"errors"
	"fmt"
	"net/http"

	"github.com/google/uuid"
	"github.com/rs/zerolog"
	tClient "go.temporal.io/sdk/client"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

// prepareProviderSiteHandler validates Provider Admin access and Site ownership
func prepareProviderSiteHandler(
	ctx context.Context,
	dbSession *cdb.Session,
	dbUser *cdbm.User,
	org string,
	siteIDStr string,
	logger zerolog.Logger,
) (*cdbm.Site, *cutil.APIError) {
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return nil, cutil.NewAPIError(http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	if !auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole) {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return nil, cutil.NewAPIError(http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return nil, cutil.NewAPIError(http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	site, err := common.GetSiteFromIDString(ctx, nil, siteIDStr, dbSession)
	if err != nil {
		switch {
		case errors.Is(err, common.ErrInvalidID):
			return nil, cutil.NewAPIError(http.StatusBadRequest, "Failed to validate Site specified in request: invalid ID", nil)
		case errors.Is(err, cdb.ErrDoesNotExist):
			return nil, cutil.NewAPIError(http.StatusBadRequest, "Site specified in request does not exist", nil)
		default:
			logger.Error().Err(err).Msg("error retrieving Site from DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Site specified in request due to DB error", nil)
		}
	}

	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return nil, cutil.NewAPIError(http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	return site, nil
}

// getSiteTemporalClient retrieves a Site's Temporal client and maps lookup
// failures to an API error. Callers must validate access to the Site first.
func getSiteTemporalClient(
	scp *sc.ClientPool,
	siteID uuid.UUID,
	logger zerolog.Logger,
) (tClient.Client, *cutil.APIError) {
	stc, err := scp.GetClientByID(siteID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	return stc, nil
}

// prepareFlowHandler validates Provider Admin access to a Site, requires Flow,
// and retrieves the Site's Temporal client.
func prepareFlowHandler(
	ctx context.Context,
	dbSession *cdb.Session,
	scp *sc.ClientPool,
	dbUser *cdbm.User,
	org string,
	siteIDStr string,
	logger zerolog.Logger,
) (*cdbm.Site, tClient.Client, *cutil.APIError) {
	site, apiErr := prepareProviderSiteHandler(ctx, dbSession, dbUser, org, siteIDStr, logger)
	if apiErr != nil {
		return nil, nil, apiErr
	}

	if site.Config == nil || !site.Config.Flow {
		logger.Warn().Msg("site does not have NICo Flow enabled")
		return nil, nil, cutil.NewAPIError(http.StatusPreconditionFailed, "Site does not have NICo Flow enabled", nil)
	}

	stc, apiErr := getSiteTemporalClient(scp, site.ID, logger)
	if apiErr != nil {
		return nil, nil, apiErr
	}

	return site, stc, nil
}
