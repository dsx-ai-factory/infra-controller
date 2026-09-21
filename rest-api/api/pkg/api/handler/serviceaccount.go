// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"net/http"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	cauth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
)

// GetCurrentServiceAccountHandler is the API Handler for getting the current Service Account
type GetCurrentServiceAccountHandler struct {
	dbSession *cdb.Session
}

// NewGetCurrentServiceAccountHandler initializes and returns a new handler for getting the current Service Account
func NewGetCurrentServiceAccountHandler(dbSession *cdb.Session) GetCurrentServiceAccountHandler {
	return GetCurrentServiceAccountHandler{
		dbSession: dbSession,
	}
}

// Handle godoc
// @Summary Retrieve information about the current Service Account
// @Description Retrieve information about the current Service Account. If it does not exist, it will be created.
// @Tags serviceaccount
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of organization"
// @Success 200 {object} model.APIServiceAccount
// @Router /v2/org/{org}/nico/service-account/current [get]
func (gcsah GetCurrentServiceAccountHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ServiceAccount", "GetCurrent", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	serviceAccountEnabled := cauth.GetIsServiceAccountFromContext(c)

	if !serviceAccountEnabled {
		logger.Info().Msg("service account is disabled for this org")

		return c.JSON(http.StatusOK, model.NewAPIServiceAccount(false, nil, nil))
	}

	// Get or create InfrastructureProvider for this org
	ipDAO := cdbm.NewInfrastructureProviderDAO(gcsah.dbSession)

	var ip *cdbm.InfrastructureProvider

	ips, err := ipDAO.GetAllByOrg(ctx, nil, org, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Infrastructure Provider for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Infrastructure Provider for org, DB error", nil)
	}

	var serr error
	if len(ips) == 0 {
		// Create Infrastructure Provider
		ip, serr = ipDAO.Create(ctx, nil, cdbm.InfrastructureProviderCreateInput{
			Name:           org,
			Org:            org,
			OrgDisplayName: cutil.GetPtr(org),
			CreatedBy:      dbUser.ID,
		})
		if serr != nil {
			logger.Error().Err(serr).Msg("error creating Infrastructure Provider DB entity")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create Infrastructure Provider for org, DB error", nil)
		}
	} else {
		ip = &ips[0]
	}

	// Get or create Tenant for this org
	tnDAO := cdbm.NewTenantDAO(gcsah.dbSession)

	var tn *cdbm.Tenant

	tns, _, err := tnDAO.GetAll(ctx, nil, cdbm.TenantFilterInput{Orgs: []string{org}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant for org, DB error", nil)
	}

	if len(tns) == 0 {
		// Create Tenant without legacy capability flags; capability is managed on TenantAccount.
		tn, serr = tnDAO.Create(ctx, nil, cdbm.TenantCreateInput{
			Name:           org,
			Org:            org,
			OrgDisplayName: cutil.GetPtr(org),
			CreatedBy:      dbUser.ID,
		})
		if serr != nil {
			logger.Error().Err(serr).Msg("error creating Tenant DB entity")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create Tenant for org, DB error", nil)
		}
	} else {
		tn = &tns[0]
	}

	accountConfig := &cdbm.TenantAccountConfig{TargetedInstanceCreation: true}

	// Create Tenant Account if it doesn't exist
	taDAO := cdbm.NewTenantAccountDAO(gcsah.dbSession)
	tas, _, err := taDAO.GetAll(ctx, nil, cdbm.TenantAccountFilterInput{
		InfrastructureProviderID: &ip.ID,
		TenantIDs:                []uuid.UUID{tn.ID},
	}, cdbp.PageInput{}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Tenant Account for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant Account for org's Tenant, DB error", nil)
	}

	if len(tas) == 0 {
		sdDAO := cdbm.NewStatusDetailDAO(gcsah.dbSession)
		serr = cdb.WithTx(ctx, gcsah.dbSession, func(tx *cdb.Tx) error {
			ta, derr := taDAO.Create(ctx, tx, cdbm.TenantAccountCreateInput{
				AccountNumber:             common.GenerateAccountNumber(),
				TenantID:                  &tn.ID,
				TenantOrg:                 org,
				InfrastructureProviderID:  ip.ID,
				InfrastructureProviderOrg: ip.Org,
				Status:                    cdbm.TenantAccountStatusReady,
				Config:                    accountConfig,
				CreatedBy:                 dbUser.ID,
			})
			if derr != nil {
				logger.Error().Err(derr).Msg("error creating Tenant Account DB entity")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Tenant Account for org's Tenant, DB error", nil)
			}

			_, derr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{
				EntityID: ta.ID.String(),
				Status:   cdbm.TenantAccountStatusReady,
				Message:  cutil.GetPtr("service account enabled, tenant account ready"),
			})
			if derr != nil {
				logger.Error().Err(derr).Msg("error creating Status Detail for Tenant Account")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for org's Tenant Account, DB error", nil)
			}

			return nil
		})
		if serr != nil {
			return common.HandleTxError(c, logger, serr, "Failed to create Tenant Account, DB transaction error")
		}
	} else if tas[0].Status != cdbm.TenantAccountStatusReady || !tas[0].Config.TargetedInstanceCreation {
		statusChanged := tas[0].Status != cdbm.TenantAccountStatusReady
		sdDAO := cdbm.NewStatusDetailDAO(gcsah.dbSession)
		serr = cdb.WithTx(ctx, gcsah.dbSession, func(tx *cdb.Tx) error {
			_, derr := taDAO.Update(ctx, tx, cdbm.TenantAccountUpdateInput{
				TenantAccountID: tas[0].ID,
				Status:          cutil.GetPtr(cdbm.TenantAccountStatusReady),
				Config: &cdbm.TenantAccountConfig{
					TargetedInstanceCreation: true,
				},
			})
			if derr != nil {
				logger.Error().Err(derr).Msg("error updating Tenant Account capabilities for org")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Tenant Account capabilities for org, DB error", nil)
			}

			if statusChanged {
				_, derr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{
					EntityID: tas[0].ID.String(),
					Status:   cdbm.TenantAccountStatusReady,
					Message:  cutil.GetPtr("service account enabled, tenant account ready"),
				})
				if derr != nil {
					logger.Error().Err(derr).Msg("error creating Status Detail for Tenant Account")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for org's Tenant Account, DB error", nil)
				}
			}

			return nil
		})
		if serr != nil {
			return common.HandleTxError(c, logger, serr, "Failed to update Tenant Account, DB transaction error")
		}
	}

	// Create response
	apiServiceAccount := model.NewAPIServiceAccount(serviceAccountEnabled, ip, tn)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiServiceAccount)
}
