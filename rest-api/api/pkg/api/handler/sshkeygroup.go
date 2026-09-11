// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"

	"go.opentelemetry.io/otel/attribute"
	temporalClient "go.temporal.io/sdk/client"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"

	"github.com/labstack/echo/v4"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	common "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"

	sshKeyGroupWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/sshkeygroup"
)

// ~~~~~ Create Handler ~~~~~ //

// CreateSSHKeyGroupHandler is the API Handler for creating new SSH Key Group
type CreateSSHKeyGroupHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewCreateSSHKeyGroupHandler initializes and returns a new handler for creating SSH Key Group
func NewCreateSSHKeyGroupHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) CreateSSHKeyGroupHandler {
	return CreateSSHKeyGroupHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Create an SSH Key Group
// @Description Create an SSH Key Group for the org.
// @Tags SSHKeyGroup
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APISSHKeyGroupCreateRequest true "SSH Key Group create request"
// @Success 201 {object} model.APISSHKeyGroup
// @Router /v2/org/{org}/nico/sshkeygroup [post]
func (cskgh CreateSSHKeyGroupHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("SSHKeyGroup", "Create", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate the tenant for which this SSH Key Group is being created
	tenant, err := common.GetTenantForOrg(ctx, nil, cskgh.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			logger.Warn().Err(err).Msg("Org does not have a Tenant associated")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("unable to retrieve Tenant for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant for org", nil)
	}

	// Validate role, only Tenant Admins are allowed to create SSH Key Group
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Validate request
	// Bind request data to API model
	apiRequest := model.APISSHKeyGroupCreateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	cotel.SetAttribute(handlerSpan, attribute.String("name", apiRequest.Name))

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating SSH Key Group creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating SSH Key Group creation request data", verr)
	}

	// check for name uniqueness for the tenant, ie, Tenant cannot have another SSH Key Group with same name
	skgDAO := cdbm.NewSSHKeyGroupDAO(cskgh.dbSession)
	skgs, tot, err := skgDAO.GetAll(
		ctx,
		nil,
		cdbm.SSHKeyGroupFilterInput{
			Names:     []string{apiRequest.Name},
			TenantIDs: []uuid.UUID{tenant.ID},
		},
		cdbp.PageInput{},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("db error checking for name uniqueness of tenant SSH Key Group")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create SSH Key Group due to DB error", nil)
	}
	if tot > 0 {
		logger.Warn().Str("tenantId", tenant.ID.String()).Str("name", apiRequest.Name).Msg("SSH Key Group with same name already exists for tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "An SSH Key Group with specified name already exists for Tenant", validation.Errors{
			"id": errors.New(skgs[0].ID.String()),
		})
	}

	// Verify or validate site
	sdDAO := cdbm.NewStatusDetailDAO(cskgh.dbSession)
	tsDAO := cdbm.NewTenantSiteDAO(cskgh.dbSession)
	skaDAO := cdbm.NewSSHKeyAssociationDAO(cskgh.dbSession)
	skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(cskgh.dbSession)

	sttsmap := map[uuid.UUID]*cdbm.TenantSite{}

	// Validate inputs (Tenant/Site access, requested Sites, requested SSH Keys)
	// outside the transaction — these are pure read-only checks and don't need
	// to be in the tx scope. The tx is opened later just for the writes.
	tss, _, err := tsDAO.GetAll(
		ctx,
		nil,
		cdbm.TenantSiteFilterInput{
			TenantIDs: []uuid.UUID{tenant.ID},
		},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("db error retrieving TenantSite records for Tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site associations for Tenant, DB error", nil)
	}

	for _, ts := range tss {
		cts := ts
		sttsmap[ts.SiteID] = &cts
	}

	rdbst := []cdbm.Site{}
	for _, stID := range apiRequest.SiteIDs {
		// Validate the site for which this SSH Key Group is being created
		site, serr := common.GetSiteFromIDString(ctx, nil, stID, cskgh.dbSession)
		if serr != nil {
			if serr == common.ErrInvalidID {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create SSH Key Group, Invalid Site ID: %s", stID), nil)
			}
			if serr == cdb.ErrDoesNotExist {
				return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Failed to create SSH Key Group, Could not find Site with ID: %s ", stID), nil)
			}
			logger.Warn().Err(serr).Str("Site ID", stID).Msg("error retrieving Site from DB by ID")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create SSH Key Group, Could not find Site with ID: %s, DB error", stID), nil)
		}

		if site.Status != cdbm.SiteStatusRegistered {
			logger.Warn().Msg(fmt.Sprintf("Unable to associate SSH Key Group to Site: %s. Site is not in Registered state", site.ID.String()))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create SSH Key Group, Site: %s specified in request is not in Registered state", site.ID.String()), nil)
		}

		// Validate the TenantSite exists for current tenant and this site
		_, ok := sttsmap[site.ID]
		if !ok {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Unable to associate SSH Key Group with Site: %s, Tenant does not have access to Site", stID), nil)
		}

		rdbst = append(rdbst, *site)
	}

	// Verify or validate SSH Key
	var rdbsk []cdbm.SSHKey
	for _, skID := range apiRequest.SSHKeyIDs {
		// Validate the SSH Key for which this SSH Key Group is being associated
		sshkey, serr := common.GetSSHKeyFromIDString(ctx, nil, skID, cskgh.dbSession)
		if serr != nil {
			if serr == common.ErrInvalidID {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create SSH Key Group, Invalid SSH Key ID: %s", skID), nil)
			}
			if serr == cdb.ErrDoesNotExist {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create SSH Key Group, Could not find SSH Key with ID: %s ", skID), nil)
			}

			logger.Warn().Err(serr).Str("SSH Key ID", skID).Msg("error retrieving SSH Key from DB by ID")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create SSH Key Group, Could not find SSH Key with ID: %s, DB error", skID), nil)
		}

		if sshkey.TenantID != tenant.ID {
			logger.Warn().Str("Tenant ID", tenant.ID.String()).Str("SSH Key ID", skID).Msg("SSH Key does not belong to current Tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create SSH Key Group, SSH Key with ID: %s does not belong to Tenant", skID), nil)
		}
		rdbsk = append(rdbsk, *sshkey)
	}

	var uskg *cdbm.SSHKeyGroup
	var dbskgsas []cdbm.SSHKeyGroupSiteAssociation
	var dbska []cdbm.SSHKeyAssociation
	dbskgsd := []cdbm.StatusDetail{}

	err = cdb.WithTx(ctx, cskgh.dbSession, func(tx *cdb.Tx) error {
		// Create SSH Key Group
		skg, derr := skgDAO.Create(
			ctx,
			tx,
			cdbm.SSHKeyGroupCreateInput{
				Name:        apiRequest.Name,
				Description: apiRequest.Description,
				TenantOrg:   org,
				TenantID:    tenant.ID,
				Status:      cdbm.SSHKeyGroupStatusSyncing,
				CreatedBy:   dbUser.ID,
			},
		)
		if derr != nil {
			logger.Error().Err(derr).Msg("unable to create the SSH Key Group record in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Error creating SSH Key Group, DB error", nil)
		}

		// Create a status detail record for the SSH Key Group
		skgsd1, derr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: skg.ID.String(), Status: cdbm.SSHKeyGroupStatusSyncing, Message: cutil.GetPtr("received SSH Key Group creation request, syncing")})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for SSH Key Group", nil)
		}
		if skgsd1 == nil {
			logger.Error().Msg("Status Detail DB entry not returned from Create")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to get new Status Detail for SSH Key Group Association", nil)
		}
		dbskgsd = append(dbskgsd, *skgsd1)

		// Create SSH Key Associations
		for _, sk := range rdbsk {
			_, serr := skaDAO.Create(ctx, tx, cdbm.SSHKeyAssociationCreateInput{
				SSHKeyID:      sk.ID,
				SSHKeyGroupID: skg.ID,
				CreatedBy:     dbUser.ID,
			})
			if serr != nil {
				logger.Error().Err(serr).Msg("unable to create the SSH Key association record in DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to associate SSH Key Group with one or more SSH Keys, DB error", nil)
			}
		}

		// Create SSH Key Group Site Associations
		for _, st := range rdbst {
			// Create SSH Key Group Site Association
			skgsa, serr := skgsaDAO.Create(ctx, tx, cdbm.SSHKeyGroupSiteAssociationCreateInput{
				SSHKeyGroupID: skg.ID,
				SiteID:        st.ID,
				Status:        cdbm.SSHKeyGroupSiteAssociationStatusSyncing,
				CreatedBy:     dbUser.ID,
			})
			if serr != nil {
				logger.Error().Err(serr).Msg("unable to create the SSH Key Group association record in DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to associate SSH Key Group with one or more Sites, DB error", nil)
			}

			// Create Status details
			_, serr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: skgsa.ID.String(), Status: cdbm.SSHKeyGroupSiteAssociationStatusSyncing, Message: cutil.GetPtr("received SSH Key Group Association create request, syncing")})
			if serr != nil {
				logger.Error().Err(serr).Msg("error creating Status Detail DB entry")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for SSH Key Group Association", nil)
			}
		}

		// Update SSH Key Group hash version
		// Get hash version for current SSH Key Group using SSH Key Group Association and SSH Key IDs
		uskg, derr = skgDAO.GenerateAndUpdateVersion(ctx, tx, skg.ID)
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating version for created SSH Key Group")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to set version for created SSH Key Group, DB error", nil)
		}

		// If there are no SSH Key Group Associations, then we can mark the SSH Key Group as synced
		if len(rdbst) == 0 {
			// Update SSH Key Group status to synced
			uskg, derr = skgDAO.Update(
				ctx,
				tx,
				cdbm.SSHKeyGroupUpdateInput{
					SSHKeyGroupID: skg.ID,
					Status:        cutil.GetPtr(cdbm.SSHKeyGroupStatusSynced),
				},
			)
			if derr != nil {
				logger.Error().Err(derr).Msg("unable to update the SSH Key Group record in DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update SSH Key Group, DB error", nil)
			}

			// Create a status detail record for the SSH Key Group
			skgsd2, serr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: skg.ID.String(), Status: cdbm.SSHKeyGroupStatusSynced, Message: cutil.GetPtr("SSH Key Group has successfully been synced to all Sites")})
			if serr != nil {
				logger.Error().Err(serr).Msg("error creating Status Detail DB entry")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for SSH Key Group", nil)
			}
			if skgsd2 == nil {
				logger.Error().Msg("Status Detail DB entry not returned from Create")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to get new Status Detail for SSH Key Group Association", nil)
			}
			dbskgsd = append(dbskgsd, *skgsd2)
		}

		// Retrieve SSH Key Group Association details
		dbskgsas, _, derr = skgsaDAO.GetAll(ctx, tx, cdbm.SSHKeyGroupSiteAssociationFilterInput{SSHKeyGroupIDs: []uuid.UUID{skg.ID}}, cdbp.PageInput{
			Limit: cutil.GetPtr(cdbp.TotalLimit),
			OrderBy: &cdbp.OrderBy{
				Field: "created",
				Order: cdbp.OrderAscending,
			},
		}, []string{cdbm.SiteRelationName})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving SSH Key Group association from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve SSH Key Group Site associations from DB", nil)
		}

		// Retrieve SSH Key Association details
		dbska, _, derr = skaDAO.GetAll(ctx, tx, cdbm.SSHKeyAssociationFilterInput{SSHKeyGroupIDs: []uuid.UUID{skg.ID}}, cdbp.PageInput{
			Limit: cutil.GetPtr(cdbp.TotalLimit),
			OrderBy: &cdbp.OrderBy{
				Field: "created",
				Order: cdbp.OrderAscending,
			},
		}, []string{cdbm.SSHKeyRelationName})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving SSH Key association from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve SSH Key Association from DB", nil)
		}

		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to create SSH Key Group, DB transaction error")
	}

	// Trigger workflows to sync SSHKeyGroup with various Sites
	for _, skgsa := range dbskgsas {
		// Trigger workflow to sync SSH Key Group
		wid, err := sshKeyGroupWorkflow.ExecuteSyncSSHKeyGroupWorkflow(ctx, cskgh.tc, skgsa.SiteID, skgsa.SSHKeyGroupID, *skgsa.Version)
		if err != nil {
			// Log error but continue, unsynced groups will be triggered by inventory
			logger.Error().Err(err).Msg("failed to execute sync SSH Key Group workflow")
			continue
		}

		logger.Info().Str("Workflow ID", *wid).Str("Site ID", skgsa.SiteID.String()).Msg("triggered SSH Key Group sync workflow")
	}

	// Create response
	apiskg := model.NewAPISSHKeyGroup(uskg, dbskgsas, sttsmap, dbska, dbskgsd)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusCreated, apiskg)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateSSHKeyGroupHandler is the API Handler for updating an SSH Key Group
type UpdateSSHKeyGroupHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewUpdateSSHKeyGroupHandler initializes and returns a new handler for updating SSH Key Group
func NewUpdateSSHKeyGroupHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) UpdateSSHKeyGroupHandler {
	return UpdateSSHKeyGroupHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Update an existing SSH Key Group
// @Description Update an existing SSH Key Group for the org
// @Tags SSHKeyGroup
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of SSH Key Group"
// @Param message body model.APISSHKeyGroupUpdateRequest true "SSH Key Group update request"
// @Success 200 {object} model.SSHKeyGroup
// @Router /v2/org/{org}/nico/sshkeygroup/{id} [patch]
func (uskgh UpdateSSHKeyGroupHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("SSHKeyGroup", "Update", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate the tenant for which this SSH Key Group is being created
	tenant, err := common.GetTenantForOrg(ctx, nil, uskgh.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			logger.Warn().Err(err).Msg("Org does not have a Tenant associated")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("unable to retrieve Tenant for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant for org", nil)
	}

	// Validate role, only Tenant Admins are allowed to update SSH Key Group
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get SSH Key Group ID from URL param
	sshKeyGroupStrID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("sshkeygroup_id", sshKeyGroupStrID))

	// Check or valdiate SSH Key Group exists
	skg, err := common.GetSSHKeyGroupFromIDString(ctx, nil, sshKeyGroupStrID, uskgh.dbSession, nil)
	if err != nil {
		if err == common.ErrInvalidID {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to update SSH Key Group, Invalid SSH Key ID: %s", sshKeyGroupStrID), nil)
		}

		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Failed to update SSH Key Group, Could not find SSH Key Group with ID: %s ", sshKeyGroupStrID), nil)
		}

		logger.Warn().Err(err).Str("SSH Key Group ID", sshKeyGroupStrID).Msg("error retrieving SSH Key Group from DB by ID")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to update SSH Key Group, Could not find SSH Key Group with ID: %s, DB error", sshKeyGroupStrID), nil)
	}

	// Check SSH Key Group belongs to the Tenant
	if skg.TenantID != tenant.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "SSH Key Group does not belong to current Tenant", nil)
	}

	// Check SSH Key Group if it is currently in deleting state
	if skg.Status == cdbm.SSHKeyGroupStatusDeleting {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "SSH Key Group is being deleted and cannot be modified", nil)
	}

	// Validate request
	// Bind request data to API model
	apiRequest := model.APISSHKeyGroupUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating SSH Key Group update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating SSH Key Group update data", verr)
	}

	// Verify version with current one
	if *skg.Version != *apiRequest.Version {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Version for SSH Key Group in request does not match with current SSH Key Group. Please fetch latest object before updating.", nil)
	}

	skgDAO := cdbm.NewSSHKeyGroupDAO(uskgh.dbSession)
	// Check for name uniqueness for the tenant, ie, tenant cannot have another SSH Key Group with same name
	if apiRequest.Name != nil && *apiRequest.Name != skg.Name {
		skgs, tot, serr := skgDAO.GetAll(
			ctx,
			nil,
			cdbm.SSHKeyGroupFilterInput{
				Names:     []string{*apiRequest.Name},
				TenantIDs: []uuid.UUID{tenant.ID},
			},
			cdbp.PageInput{},
			nil,
		)
		if serr != nil {
			logger.Error().Err(serr).Msg("db error checking for name uniqueness of tenant SSH Key Group")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to update SSH Key Group, DB error", nil)
		}
		if tot > 0 {
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Another SSH Key Group with specified name already exists for Tenant", validation.Errors{"id": errors.New(skgs[0].ID.String())})
		}
	}

	tsDAO := cdbm.NewTenantSiteDAO(uskgh.dbSession)
	skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(uskgh.dbSession)
	sdDAO := cdbm.NewStatusDetailDAO(uskgh.dbSession)
	skaDAO := cdbm.NewSSHKeyAssociationDAO(uskgh.dbSession)

	// Pre-validate the Tenant's site access map outside the transaction (e.g.
	// does the tenant own the site they're targeting?), which doesn't need to
	// be in the tx scope; we open it later for lock + writes.
	sttsmap := map[uuid.UUID]*cdbm.TenantSite{}
	tss, _, err := tsDAO.GetAll(
		ctx,
		nil,
		cdbm.TenantSiteFilterInput{
			TenantIDs: []uuid.UUID{tenant.ID},
		},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("db error retrieving TenantSite records for Tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site associations for Tenant, DB error", nil)
	}
	for _, ts := range tss {
		cts := ts
		sttsmap[ts.SiteID] = &cts
	}

	skgsasToSync := []cdbm.SSHKeyGroupSiteAssociation{}
	skgsasToDelete := []cdbm.SSHKeyGroupSiteAssociation{}

	var dbskgsd []cdbm.StatusDetail
	var dbskgsas []cdbm.SSHKeyGroupSiteAssociation
	var dbska []cdbm.SSHKeyAssociation

	err = cdb.WithTx(ctx, uskgh.dbSession, func(tx *cdb.Tx) error {
		// Acquire an advisory lock on the SSH Key Group on which there could be contention
		// this lock is released when the transaction commits or rollsback
		derr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(skg.ID.String()), nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("Failed to acquire advisory lock on SSH Key Group")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update SSH Key Group, could not acquire DB lock", nil)
		}

		// Re-read the SSH Key Group inside the locked tx and re-validate the
		// version. The pre-flight check above ran without a lock, so two
		// concurrent PATCHes can both pass it; without this recheck, the
		// second one would overwrite the first's changes.
		latestSkg, derr := skgDAO.GetByID(ctx, tx, skg.ID, nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("error re-reading SSH Key Group inside locked tx")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update SSH Key Group, DB error", nil)
		}
		if *latestSkg.Version != *apiRequest.Version {
			return cutil.NewAPIError(http.StatusForbidden, "Version for SSH Key Group in request does not match with current SSH Key Group. Please fetch latest object before updating.", nil)
		}
		skg = latestSkg

		// Processing SSH Key Group Site Association
		// Always load existing site/key associations so partial PATCHes (key-only
		// or site-only) still see the other side's current state — sync logic
		// below depends on both maps being populated.
		existingSiteAssociationIDMap := map[string]cdbm.SSHKeyGroupSiteAssociation{}
		newSiteAssociationIDMap := map[string]bool{}
		reportedSiteAssociationIDMap := map[string]bool{}
		deletingSiteAssociationIDMap := map[uuid.UUID]bool{}

		existingGroupAssociations, _, serr := skgsaDAO.GetAll(ctx, tx, cdbm.SSHKeyGroupSiteAssociationFilterInput{SSHKeyGroupIDs: []uuid.UUID{skg.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if serr != nil {
			logger.Error().Err(serr).Msg("error retrieving SSH Key Group association entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve SSH Key Group association, DB error", nil)
		}
		for _, sga := range existingGroupAssociations {
			existingSiteAssociationIDMap[sga.SiteID.String()] = sga
		}

		if apiRequest.SiteIDs != nil {
			// Preparing new Site Association map
			for _, stID := range apiRequest.SiteIDs {
				reportedSiteAssociationIDMap[stID] = true

				_, efound := existingSiteAssociationIDMap[stID]
				if !efound {
					newSiteAssociationIDMap[stID] = true
				}
			}

			// Validating and creating new SSH Key Group's Site Association
			for stID, _ := range newSiteAssociationIDMap {
				// Validate the site for which this SSH Key Group is being created
				site, serr := common.GetSiteFromIDString(ctx, nil, stID, uskgh.dbSession)
				if serr != nil {
					if serr == common.ErrInvalidID {
						return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Failed to update SSH Key Group, Invalid Site ID: %s", stID), nil)
					}
					if serr == cdb.ErrDoesNotExist {
						return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Failed to update SSH Key Group, Could not find Site with ID: %s ", stID), nil)
					}
					logger.Warn().Err(serr).Str("Site ID", stID).Msg("error retrieving Site from DB by ID")
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Failed to update SSH Key Group, Could not find Site with ID: %s, DB error", stID), nil)
				}

				if site.Status != cdbm.SiteStatusRegistered {
					logger.Warn().Msg(fmt.Sprintf("Unable to associate SSH Key Group to Site: %s. Site is not in Registered state", site.ID.String()))
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Failed to update SSH Key Group, The Site with ID: %s where this SSH Key Group is being created is not in Registered state", site.ID.String()), nil)
				}

				// Validate the TenantSite exists for current tenant and this site
				_, ok := sttsmap[site.ID]
				if !ok {
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Unable to associate SSH Key Group with Site: %s, Tenant does not have access to Site", stID), nil)
				}

				// Create SSH Key Group Association
				skgsa, serr := skgsaDAO.Create(ctx, tx, cdbm.SSHKeyGroupSiteAssociationCreateInput{
					SSHKeyGroupID: skg.ID,
					SiteID:        site.ID,
					Status:        cdbm.SSHKeyGroupSiteAssociationStatusSyncing,
					CreatedBy:     dbUser.ID,
				})
				if serr != nil {
					logger.Error().Err(serr).Msg("unable to create the SSH Key Group association record in DB")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to associate SSH Key Group with one or more Sites, DB error", nil)
				}

				// Create Status details
				_, serr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: skgsa.ID.String(), Status: cdbm.SSHKeyGroupSiteAssociationStatusSyncing, Message: cutil.GetPtr("received SSH Key Group Association create request, syncing")})
				if serr != nil {
					logger.Error().Err(serr).Msg("error creating Status Detail DB entry")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for SSH Key Group Association", nil)
				}
			}

			// Preparing deleting SSH Key Group's Site Association map
			for stID, sga := range existingSiteAssociationIDMap {
				_, rfound := reportedSiteAssociationIDMap[stID]
				if !rfound {
					deletingSiteAssociationIDMap[sga.ID] = true
				}
			}

			// Updating existing SSH Key Group Association status as deleting
			for sgaID, _ := range deletingSiteAssociationIDMap {
				_, serr := skgsaDAO.Update(ctx, tx, cdbm.SSHKeyGroupSiteAssociationUpdateInput{
					ID:     sgaID,
					Status: cutil.GetPtr(cdbm.SSHKeyGroupSiteAssociationStatusDeleting),
				})
				if serr != nil {
					logger.Error().Err(serr).Msg("unable to update the SSH Key Group association status record in DB")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update SSH Key Group association status with one or more Sites, DB error", nil)
				}

				// Create Status details
				_, serr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: sgaID.String(), Status: cdbm.SSHKeyGroupSiteAssociationStatusDeleting, Message: cutil.GetPtr("received SSH Key Group Association update request, deleting")})
				if serr != nil {
					logger.Error().Err(serr).Msg("error creating Status Detail DB entry")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for SSH Key Group Association", nil)
				}
			}
		}

		// Processing SSH Key Association
		existingKeyAssociationIDMap := map[string]cdbm.SSHKeyAssociation{}
		newSSHKeyIDMap := map[string]bool{}
		reportedSSHKeyIDMap := map[string]bool{}
		deletingKeyAssociationIDMap := map[uuid.UUID]bool{}

		existingSSHKeyAssociations, _, serr := skaDAO.GetAll(ctx, tx, cdbm.SSHKeyAssociationFilterInput{SSHKeyGroupIDs: []uuid.UUID{skg.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if serr != nil {
			logger.Error().Err(serr).Msg("error retrieving SSH Key association entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve SSH Key association, DB error", nil)
		}
		for _, ska := range existingSSHKeyAssociations {
			existingKeyAssociationIDMap[ska.SSHKeyID.String()] = ska
		}

		if apiRequest.SSHKeyIDs != nil {
			// Preparing new SSH Key map
			for _, skID := range apiRequest.SSHKeyIDs {
				reportedSSHKeyIDMap[skID] = true
				_, found := existingKeyAssociationIDMap[skID]
				if !found {
					newSSHKeyIDMap[skID] = true
				}
			}

			// Validating SSH Key and creating new SSH Key Association
			for skID := range newSSHKeyIDMap {
				// Validate the SSH Key for which this SSH Key Group is being associated
				sshkey, serr := common.GetSSHKeyFromIDString(ctx, nil, skID, uskgh.dbSession)
				if serr != nil {
					if serr == common.ErrInvalidID {
						return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Failed to update SSH Key Group, Invalid SSH Key ID: %s", skID), nil)
					}
					if serr == cdb.ErrDoesNotExist {
						return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Failed to update SSH Key Group, Could not find SSH Key with ID: %s ", skID), nil)
					}
					logger.Warn().Err(serr).Str("SSH Key ID", skID).Msg("error retrieving SSH Key from DB by ID")
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Failed to update SSH Key Group, Could not find SSH Key with ID: %s, DB error", skID), nil)
				}

				if sshkey.TenantID != tenant.ID {
					logger.Warn().Str("Tenant ID", tenant.ID.String()).Str("SSH Key ID", skID).Msg("SSH Key does not belong to current Tenant")
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Failed to update SSH Key Group, SSH Key with ID: %s does not belong to Tenant", skID), nil)
				}

				// Create SSH Key Association
				_, serr = skaDAO.Create(ctx, tx, cdbm.SSHKeyAssociationCreateInput{
					SSHKeyID:      sshkey.ID,
					SSHKeyGroupID: skg.ID,
					CreatedBy:     dbUser.ID,
				})
				if serr != nil {
					logger.Error().Err(serr).Msg("unable to create the SSH Key association record in DB")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to associate SSH Key Group with one or more SSH Keys, DB error", nil)
				}
			}

			// Preparing deleting SSH Key Association map
			for skID, ska := range existingKeyAssociationIDMap {
				_, nfound := newSSHKeyIDMap[skID]
				_, rfound := reportedSSHKeyIDMap[skID]
				if !nfound && !rfound {
					deletingKeyAssociationIDMap[ska.ID] = true
				}
			}

			// Deleting existing SSH Key Association
			for skaID := range deletingKeyAssociationIDMap {
				serr := skaDAO.Delete(ctx, tx, skaID)
				if serr != nil {
					logger.Error().Err(serr).Msg("unable to delete the SSH Key association record in DB")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update SSH Key Group, unable to delete SSH Key Group association with one or more SSH Key, DB error", nil)
				}
			}
		}

		// Updating existing SSH Key Group Site Association to be syncing if new SSH key added or removed
		if len(newSSHKeyIDMap) > 0 || len(deletingKeyAssociationIDMap) > 0 {
			// Preparing updating SSH Key Group's Site Association status as 'Syncing'
			for stID, sga := range existingSiteAssociationIDMap {
				// Skip associations already in Deleting; flipping them back to
				// Syncing would re-activate sites pending removal.
				if sga.Status == cdbm.SSHKeyGroupSiteAssociationStatusDeleting {
					continue
				}
				_, dfound := deletingSiteAssociationIDMap[sga.ID]
				_, nfound := newSiteAssociationIDMap[stID]
				if !dfound && !nfound {
					_, serr := skgsaDAO.Update(ctx, tx, cdbm.SSHKeyGroupSiteAssociationUpdateInput{ID: sga.ID, Status: cutil.GetPtr(cdbm.SSHKeyGroupSiteAssociationStatusSyncing)})
					if serr != nil {
						logger.Error().Err(serr).Msg("failed to update the SSH Key Group association status record in DB")
						return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update SSH Key Group Association status for one or more Sites, DB error", nil)
					}
				}
			}
		}

		siteAssociationChanged := len(newSiteAssociationIDMap) > 0 || len(deletingSiteAssociationIDMap) > 0
		keyAssociationChanged := len(newSSHKeyIDMap) > 0 || len(deletingKeyAssociationIDMap) > 0

		syncRequired := (len(existingKeyAssociationIDMap) != 0 && siteAssociationChanged) || (len(existingSiteAssociationIDMap) != 0 && keyAssociationChanged) || (siteAssociationChanged && keyAssociationChanged)

		// Update SSH Key Group in DB
		if apiRequest.Name != nil || apiRequest.Description != nil {
			skg, derr = skgDAO.Update(
				ctx,
				tx,
				cdbm.SSHKeyGroupUpdateInput{
					SSHKeyGroupID: skg.ID,
					Name:          apiRequest.Name,
					Description:   apiRequest.Description,
				},
			)
			if derr != nil {
				logger.Error().Err(derr).Msg("unable to update the SSH Key Group record in DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update SSH Key Group, DB error", nil)
			}
		}

		if siteAssociationChanged || keyAssociationChanged {
			// Update SSH Key Group/Association versions
			skg, derr = skgDAO.GenerateAndUpdateVersion(ctx, tx, skg.ID)
			if derr != nil {
				logger.Error().Err(derr).Msg("error updating current version for SSH Key Group")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to set updated version for SSH Key Group", nil)
			}
		}

		// Preparing response
		// Retrieve SSH Key Group status details
		dbskgsd, _, derr = sdDAO.GetAll(ctx, tx, cdbm.StatusDetailFilterInput{EntityIDs: []string{skg.ID.String()}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Status Details for SSH Key Group from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Status Details for SSH Key Group, DB error", nil)
		}

		// Retrieve SSH Key Group Site Association details
		dbskgsas, _, derr = skgsaDAO.GetAll(ctx, tx, cdbm.SSHKeyGroupSiteAssociationFilterInput{SSHKeyGroupIDs: []uuid.UUID{skg.ID}}, cdbp.PageInput{
			Limit: cutil.GetPtr(cdbp.TotalLimit),
			OrderBy: &cdbp.OrderBy{
				Field: "created",
				Order: cdbp.OrderAscending,
			},
		}, []string{cdbm.SiteRelationName})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving SSH Key Group association from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve SSH Key Group Site associations from DB", nil)
		}

		// Retrieve SSH Key Association details
		dbska, _, derr = skaDAO.GetAll(ctx, tx, cdbm.SSHKeyAssociationFilterInput{SSHKeyGroupIDs: []uuid.UUID{skg.ID}}, cdbp.PageInput{
			Limit: cutil.GetPtr(cdbp.TotalLimit),
			OrderBy: &cdbp.OrderBy{
				Field: "created",
				Order: cdbp.OrderAscending,
			},
		}, []string{cdbm.SSHKeyRelationName})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving SSH Key association from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve SSH Key Association from DB", nil)
		}

		if syncRequired {
			skg, derr = skgDAO.Update(
				ctx,
				tx,
				cdbm.SSHKeyGroupUpdateInput{
					SSHKeyGroupID: skg.ID,
					Status:        cutil.GetPtr(cdbm.SSHKeyGroupStatusSyncing),
				},
			)
			if derr != nil {
				logger.Error().Err(derr).Msg("unable to update the SSH Key Group record in DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update SSH Key Group, DB error", nil)
			}

			// Create a status detail record for the SSH Key Group
			_, serr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: skg.ID.String(), Status: cdbm.SSHKeyGroupStatusSyncing, Message: cutil.GetPtr("received SSH Key Group update request, syncing")})
			if serr != nil {
				logger.Error().Err(serr).Msg("error creating Status Detail DB entry")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for SSH Key Group", nil)
			}

			// Refresh status details so the response includes the row we just inserted.
			dbskgsd, _, derr = sdDAO.GetAll(ctx, tx, cdbm.StatusDetailFilterInput{EntityIDs: []string{skg.ID.String()}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)})
			if derr != nil {
				logger.Error().Err(derr).Msg("error retrieving Status Details for SSH Key Group from DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Status Details for SSH Key Group, DB error", nil)
			}
		}

		// Determine workflow targets to trigger after commit.
		//
		// When keys changed, we only populate skgsasToSync (skipping any
		// associations already in Deleting). We deliberately do NOT populate
		// skgsasToDelete here: any sites flagged for deletion earlier in this
		// PATCH had their status persisted as Deleting on line 663 and will be
		// cleaned up asynchronously by inventory reconciliation (which drives
		// delete workflows for stale Deleting associations). Triggering the
		// delete workflow immediately would race the sync workflow we just
		// queued for the remaining sites.
		//
		// When only sites changed (no key changes), there's no sync workflow
		// to race against, so we can populate both skgsasToSync (for newly
		// added sites) and skgsasToDelete (for sites flagged for deletion)
		// and trigger their workflows directly below.
		if len(newSSHKeyIDMap) > 0 || len(deletingKeyAssociationIDMap) > 0 {
			for _, skgsa := range dbskgsas {
				if skgsa.Status == cdbm.SSHKeyGroupSiteAssociationStatusDeleting {
					continue
				}

				skgsasToSync = append(skgsasToSync, skgsa)
			}
		} else {
			for _, skgsa := range dbskgsas {
				if newSiteAssociationIDMap[skgsa.SiteID.String()] {
					skgsasToSync = append(skgsasToSync, skgsa)
				}

				if deletingSiteAssociationIDMap[skgsa.ID] {
					skgsasToDelete = append(skgsasToDelete, skgsa)
				}
			}
		}

		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to update SSH Key Group, DB transaction error")
	}

	// Sync SSH Key Group across Sites
	for _, skgsa := range skgsasToSync {
		// Trigger workflow to sync SSH Key Group with Site
		wid, err := sshKeyGroupWorkflow.ExecuteSyncSSHKeyGroupWorkflow(ctx, uskgh.tc, skgsa.SiteID, skgsa.SSHKeyGroupID, *skgsa.Version)
		if err != nil {
			// Log error but continue, unsynced groups will be triggered by inventory
			logger.Error().Err(err).Msg("failed to execute sync SSH Key Group workflow")
			continue
		}

		logger.Info().Str("Workflow ID", *wid).Str("Site ID", skgsa.SiteID.String()).Msg("triggered SSH Key Group sync workflow")
	}

	for _, skgsa := range skgsasToDelete {
		// Trigger workflow to delete SSH Key Group from Site
		wid, err := sshKeyGroupWorkflow.ExecuteDeleteSSHKeyGroupWorkflow(ctx, uskgh.tc, skgsa.SiteID, skgsa.SSHKeyGroupID)
		if err != nil {
			// Log error but continue, unsynced groups will be triggered by inventory
			logger.Error().Err(err).Msg("failed to execute delete SSH Key Group workflow")
			continue
		}

		logger.Info().Str("Workflow ID", *wid).Str("Site ID", skgsa.SiteID.String()).Msg("triggered SSH Key Group delete workflow")
	}

	// Create response
	apiskg := model.NewAPISSHKeyGroup(skg, dbskgsas, sttsmap, dbska, dbskgsd)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiskg)
}

// ~~~~~ Get Handler ~~~~~ //

// GetSSHKeyGroupHandler is the API Handler for getting an SSH Key Group
type GetSSHKeyGroupHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetSSHKeyGroupHandler initializes and returns a new handler for getting SSH Key Group
func NewGetSSHKeyGroupHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetSSHKeyGroupHandler {
	return GetSSHKeyGroupHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get an SSH Key Group
// @Description Get an SSH Key Group for the org
// @Tags SSHKeyGroup
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of SSH Key Group"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Tenant'"
// @Success 200 {object} model.APISSHKeyGroup
// @Router /v2/org/{org}/nico/sshkeygroup/{id} [get]
func (gskgh GetSSHKeyGroupHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("SSHKeyGroup", "Get", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate the tenant for which this SSH Key Group is being created
	tenant, err := common.GetTenantForOrg(ctx, nil, gskgh.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			logger.Warn().Err(err).Msg("Org does not have a Tenant associated")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("unable to retrieve Tenant for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant for org", nil)
	}

	// Validate role, only Tenant Admins are allowed to get SSH Key Group
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get SSH Key Group ID from URL param
	sshKeyGroupStrID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("sshkeygroup_id", sshKeyGroupStrID))

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.SSHKeyGroupRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Check or valdiate SSH Key Group exists
	skg, err := common.GetSSHKeyGroupFromIDString(ctx, nil, sshKeyGroupStrID, gskgh.dbSession, qIncludeRelations)
	if err != nil {
		if err == common.ErrInvalidID {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to get SSH Key Group, Invalid SSH Key ID: %s", sshKeyGroupStrID), nil)
		}

		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Failed to get SSH Key Group, Could not find SSH Key Group with ID: %s ", sshKeyGroupStrID), nil)
		}

		logger.Warn().Err(err).Str("SSH Key Group ID", sshKeyGroupStrID).Msg("error retrieving SSH Key Group from DB by ID")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to get SSH Key Group, Could not find SSH Key Group with ID: %s, DB error", sshKeyGroupStrID), nil)
	}

	// Check SSH Key Group belongs to the Tenant
	if skg.TenantID != tenant.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "SSH Key Group does not belong to current Tenant", nil)
	}

	tsDAO := cdbm.NewTenantSiteDAO(gskgh.dbSession)
	skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(gskgh.dbSession)
	skaDAO := cdbm.NewSSHKeyAssociationDAO(gskgh.dbSession)
	sdDAO := cdbm.NewStatusDetailDAO(gskgh.dbSession)

	// Get all TenantSite records for the Tenant
	sttsmap := map[uuid.UUID]*cdbm.TenantSite{}

	tss, _, err := tsDAO.GetAll(
		ctx,
		nil,
		cdbm.TenantSiteFilterInput{
			TenantIDs: []uuid.UUID{tenant.ID},
		},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("db error retrieving TenantSite records for Tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site associations for Tenant, DB error", nil)
	}

	for _, ts := range tss {
		cts := ts
		sttsmap[ts.SiteID] = &cts
	}

	// Retrieve SSH Key Group status details
	dbskgsd, err := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{skg.ID.String()}, common.RECENT_STATUS_DETAIL_COUNT)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Status Details for SSH Key Group from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Status Details for SSH Key Group, DB error", nil)
	}

	// Retrieve SSH Key Group Site Association details
	dbskgsas, _, err := skgsaDAO.GetAll(ctx, nil, cdbm.SSHKeyGroupSiteAssociationFilterInput{SSHKeyGroupIDs: []uuid.UUID{skg.ID}}, cdbp.PageInput{
		Limit: cutil.GetPtr(cdbp.TotalLimit),
		OrderBy: &cdbp.OrderBy{
			Field: "created",
			Order: cdbp.OrderAscending,
		},
	}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving SSH Key Group association from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SSH Key Group Site associations from DB", nil)
	}

	// Retrieve Site with Infrastructure Provider if requested
	stDAO := cdbm.NewSiteDAO(gskgh.dbSession)
	dbstMap := map[uuid.UUID]*cdbm.Site{}
	siteIDs := []uuid.UUID{}
	for _, dbskgsa := range dbskgsas {
		siteIDs = append(siteIDs, dbskgsa.SiteID)
	}

	sts, _, err := stDAO.GetAll(ctx, nil, cdbm.SiteFilterInput{SiteIDs: siteIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, []string{cdbm.InfrastructureProviderRelationName})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site from DB", nil)
	}

	// Build site map with siteID
	for _, dbst := range sts {
		cdbst := dbst
		dbstMap[dbst.ID] = &cdbst
	}

	// Update respective site with infranstructure provider relation
	for i, _ := range dbskgsas {
		dbskgsas[i].Site = dbstMap[dbskgsas[i].SiteID]
	}

	// Retrieve SSH Key Association details
	dbska, _, err := skaDAO.GetAll(ctx, nil, cdbm.SSHKeyAssociationFilterInput{SSHKeyGroupIDs: []uuid.UUID{skg.ID}}, cdbp.PageInput{
		Limit: cutil.GetPtr(cdbp.TotalLimit),
		OrderBy: &cdbp.OrderBy{
			Field: "created",
			Order: cdbp.OrderAscending,
		},
	}, []string{cdbm.SSHKeyRelationName})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving SSH Key association from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SSH Key Association from DB", nil)
	}

	// Create response
	apiSSHKeyGroup := model.NewAPISSHKeyGroup(skg, dbskgsas, sttsmap, dbska, dbskgsd)

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiSSHKeyGroup)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllSSHKeyGroupHandler is the API Handler for retrieving all SSH Key Groups
type GetAllSSHKeyGroupHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetAllSSHKeyGroupHandler initializes and returns a new handler for retreiving all SSH Key Groups
func NewGetAllSSHKeyGroupHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllSSHKeyGroupHandler {
	return GetAllSSHKeyGroupHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get all SSH Key Groups
// @Description Get all SSH Key Group for the org
// @Tags SSHKeyGroup
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of Site"
// @Param status query string false "Filter by status" e.g. 'Pending', 'Error'"
// @Param instanceId query string true "ID of Instance"
// @Param query query string false "Query input for full text search"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Tenant'"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {array} []model.APISSHKeyGroup
// @Router /v2/org/{org}/nico/sshkeygroup [get]
func (gaskgh GetAllSSHKeyGroupHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("SSHKeyGroup", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate the tenant for which this SSH Key Group is being created
	tenant, err := common.GetTenantForOrg(ctx, nil, gaskgh.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			logger.Warn().Err(err).Msg("Org does not have a Tenant associated")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("unable to retrieve tenant for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve tenant for org", nil)
	}

	// Validate role, only Tenant Admins are allowed to get SSH Key Groups
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Validate pagination request
	pageRequest := pagination.PageRequest{}
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate pagination request attributes
	err = pageRequest.Validate(cdbm.SSHKeyGroupOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest,
			"Failed to validate pagination request data", err)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.SSHKeyGroupRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// now check siteID in query
	tsDAO := cdbm.NewTenantSiteDAO(gaskgh.dbSession)

	var site *cdbm.Site

	qSiteID := c.QueryParam("siteId")
	if qSiteID != "" {
		site, err = common.GetSiteFromIDString(ctx, nil, qSiteID, gaskgh.dbSession)
		if err != nil {
			logger.Warn().Err(err).Msg("error getting Site from query string")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Site specified in query", nil)
		}

		// Determine if tenant has access to requested site
		_, err = tsDAO.GetByTenantIDAndSiteID(ctx, nil, tenant.ID, site.ID, nil)
		if err != nil {
			if err == cdb.ErrDoesNotExist {
				return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant is not associated with Site specified in query", nil)
			}
			logger.Warn().Err(err).Msg("error retrieving Tenant Site association from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to determine if Tenant has access to Site specified in query, DB error", nil)
		}
	}

	// now check instanceID in query
	var instance *cdbm.Instance

	qInstanceID := c.QueryParam("instanceId")
	if qInstanceID != "" {
		instance, err = common.GetInstanceFromIDString(ctx, nil, qInstanceID, gaskgh.dbSession)
		if err != nil {
			logger.Warn().Err(err).Msg("error getting Instance from query string")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Instance specified in query", nil)
		}

		if instance.TenantID != tenant.ID {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Instance specified in query is not owned by Tenant", nil)
		}

		if site != nil && instance.SiteID != site.ID {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Instance specified in query is not associated with Site specified in query", nil)
		}
	}

	// Get query text for full text search from query param
	searchQuery := common.GetSearchQuery(c)
	if searchQuery != nil {
		cotel.SetAttribute(handlerSpan, attribute.String("query", *searchQuery))
	}

	// Get all SSH Key Group by Tenant
	skgIDs := []uuid.UUID{}

	skgDAO := cdbm.NewSSHKeyGroupDAO(gaskgh.dbSession)
	skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(gaskgh.dbSession)

	if instance != nil {
		// If Instance ID was specified then we only need to filter SSH Key Groups by Instance
		var skgias []cdbm.SSHKeyGroupInstanceAssociation

		skgiaDAO := cdbm.NewSSHKeyGroupInstanceAssociationDAO(gaskgh.dbSession)

		skgias, _, err = skgiaDAO.GetAll(ctx, nil, cdbm.SSHKeyGroupInstanceAssociationFilterInput{
			InstanceIDs: []uuid.UUID{instance.ID},
		}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving SSH Key Group Instance Associations from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to filter SSH Key Groups by Instance ID, DB error", nil)
		}

		for _, skgia := range skgias {
			skgIDs = append(skgIDs, skgia.SSHKeyGroupID)
		}
	} else {
		// Otherwise if Site ID was specified, we start with fetching all of Tenant's SSH Key Groups and then filter by Site
		tskgs, _, serr := skgDAO.GetAll(
			ctx,
			nil,
			cdbm.SSHKeyGroupFilterInput{
				TenantIDs: []uuid.UUID{tenant.ID},
			},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		if serr != nil {
			logger.Error().Err(serr).Msg("error retrieving SSH Key Groups for Tenant from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SSH Key Groups for Tenant, DB error", nil)
		}
		tskgIDs := []uuid.UUID{}
		for _, tskg := range tskgs {
			tskgIDs = append(tskgIDs, tskg.ID)
		}

		if site != nil {
			sttskgs, _, serr := skgsaDAO.GetAll(ctx, nil, cdbm.SSHKeyGroupSiteAssociationFilterInput{SSHKeyGroupIDs: tskgIDs, SiteID: &site.ID}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
			if serr != nil {
				logger.Error().Err(serr).Msg("error retrieving SSH Key Group Site Associations from DB")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to filter SSH Key Groups by Site ID, DB error", nil)
			}

			for _, sttskg := range sttskgs {
				skgIDs = append(skgIDs, sttskg.SSHKeyGroupID)
			}
		} else {
			skgIDs = tskgIDs
		}
	}

	// Get status from query param
	var statuses []string
	statusQuery := c.QueryParam("status")
	if statusQuery != "" {
		_, ok := cdbm.SSHKeyGroupMap[statusQuery]
		if !ok {
			logger.Warn().Msg(fmt.Sprintf("invalid value in status query: %v", statusQuery))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Status value in query", nil)
		}
		statuses = append(statuses, statusQuery)
		cotel.SetAttribute(handlerSpan, attribute.String("status", statusQuery))
	}

	// Prepare response for SSH Key Groups
	apiSSHKeyGroups := []model.APISSHKeyGroup{}
	dbskgsaMap := map[uuid.UUID][]cdbm.SSHKeyGroupSiteAssociation{}
	dbskaMap := map[uuid.UUID][]cdbm.SSHKeyAssociation{}
	dbskgsdMap := map[string][]cdbm.StatusDetail{}

	skaDAO := cdbm.NewSSHKeyAssociationDAO(gaskgh.dbSession)
	sdDAO := cdbm.NewStatusDetailDAO(gaskgh.dbSession)

	// Get SSH Key Groups
	dbskgs, total, err := skgDAO.GetAll(
		ctx,
		nil,
		cdbm.SSHKeyGroupFilterInput{
			SSHKeyGroupIDs: skgIDs,
			Statuses:       statuses,
			SearchQuery:    searchQuery,
		},
		cdbp.PageInput{
			Offset:  pageRequest.Offset,
			Limit:   pageRequest.Limit,
			OrderBy: pageRequest.OrderBy,
		},
		qIncludeRelations,
	)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving SSH Key Groups from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SSH Key Groups, DB error", nil)
	}

	// Get all TenantSite records for the Tenant
	sttsmap := map[uuid.UUID]*cdbm.TenantSite{}

	tss, _, err := tsDAO.GetAll(
		ctx,
		nil,
		cdbm.TenantSiteFilterInput{
			TenantIDs: []uuid.UUID{tenant.ID},
		},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("db error retrieving TenantSite records for Tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site associations for Tenant, DB error", nil)
	}

	for _, ts := range tss {
		cts := ts
		sttsmap[ts.SiteID] = &cts
	}

	// Retrieve SSH Key Group status details
	dbskgStrIDs := []string{}
	for _, skgID := range skgIDs {
		dbskgStrIDs = append(dbskgStrIDs, skgID.String())
	}

	dbskgssd, err := sdDAO.GetRecentByEntityIDs(ctx, nil, dbskgStrIDs, common.RECENT_STATUS_DETAIL_COUNT)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Status Details for SSH Key Group from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Status Details for SSH Key Group, DB error", nil)
	}

	for _, skgssd := range dbskgssd {
		cskgssd := skgssd
		dbskgsdMap[skgssd.EntityID] = append(dbskgsdMap[skgssd.EntityID], cskgssd)
	}

	// Retrieve SSH Key Group Site Association details
	dbskgsas, _, err := skgsaDAO.GetAll(ctx, nil, cdbm.SSHKeyGroupSiteAssociationFilterInput{SSHKeyGroupIDs: skgIDs}, cdbp.PageInput{
		Limit: cutil.GetPtr(cdbp.TotalLimit),
		OrderBy: &cdbp.OrderBy{
			Field: "created",
			Order: cdbp.OrderAscending,
		},
	}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving SSH Key Group Site association from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SSH Key Group Site associations from DB", nil)
	}

	// Retrieve Site with Infrastructure Provider if requested
	stDAO := cdbm.NewSiteDAO(gaskgh.dbSession)
	dbstMap := map[uuid.UUID]*cdbm.Site{}

	siteIDs := []uuid.UUID{}
	for _, dbskgsa := range dbskgsas {
		siteIDs = append(siteIDs, dbskgsa.SiteID)
	}

	sts, _, err := stDAO.GetAll(ctx, nil, cdbm.SiteFilterInput{SiteIDs: siteIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, []string{cdbm.InfrastructureProviderRelationName})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site from DB", nil)
	}

	// Build site map with siteID
	for _, dbst := range sts {
		cdbst := dbst
		dbstMap[dbst.ID] = &cdbst
	}

	for _, dbskgsa := range dbskgsas {
		cdbskgsa := dbskgsa
		cdbskgsa.Site = dbstMap[cdbskgsa.SiteID]
		dbskgsaMap[dbskgsa.SSHKeyGroupID] = append(dbskgsaMap[dbskgsa.SSHKeyGroupID], cdbskgsa)
	}

	// Retrieve SSH Key Association details
	dbskas, _, err := skaDAO.GetAll(ctx, nil, cdbm.SSHKeyAssociationFilterInput{SSHKeyGroupIDs: skgIDs}, cdbp.PageInput{
		Limit: cutil.GetPtr(cdbp.TotalLimit),
		OrderBy: &cdbp.OrderBy{
			Field: "created",
			Order: cdbp.OrderAscending,
		},
	}, []string{cdbm.SSHKeyRelationName})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving SSH Key association from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SSH Key Association from DB", nil)
	}

	for _, dbska := range dbskas {
		cdbska := dbska
		dbskaMap[dbska.SSHKeyGroupID] = append(dbskaMap[dbska.SSHKeyGroupID], cdbska)
	}

	// Preparing response for each SSH Key Group
	for _, skg := range dbskgs {
		dbSSHKeyGroup := skg

		// Get SSH Key Group Site Association
		dbskgsas := dbskgsaMap[skg.ID]

		// Get SSH Key Association details
		dbskas := dbskaMap[skg.ID]

		// Get SSH Key Group status details
		dbskgsd := dbskgsdMap[skg.ID.String()]

		apiskg := model.NewAPISSHKeyGroup(&dbSSHKeyGroup, dbskgsas, sttsmap, dbskas, dbskgsd)
		apiSSHKeyGroups = append(apiSSHKeyGroups, *apiskg)
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

	return c.JSON(http.StatusOK, apiSSHKeyGroups)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteSSHKeyGroupHandler is the API Handler for deleting an SSH Key Group
type DeleteSSHKeyGroupHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewDeleteSSHKeyGroupHandler initializes and returns a new handler for deleting an SSH Key Group
func NewDeleteSSHKeyGroupHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) DeleteSSHKeyGroupHandler {
	return DeleteSSHKeyGroupHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Delete an SSHKeyGroup
// @Description Delete an SSHKeyGroup from the org
// @Tags SSHKeyGroup
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of SSHKeyGroup"
// @Success 202
// @Router /v2/org/{org}/nico/sshkeygroup/{id} [delete]
func (dskgh DeleteSSHKeyGroupHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("SSHKeyGroup", "Delete", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate the tenant for which this SSHKeyGroup is being created
	tenant, err := common.GetTenantForOrg(ctx, nil, dskgh.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			logger.Warn().Err(err).Msg("Org does not have a Tenant associated")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("unable to retrieve Tenant for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant for org", nil)
	}

	// Validate role, only Tenant Admins are allowed to delete SSH Key Group
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get ID from URL param
	sshKeyGroupStrID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("sshkeygroup_id", sshKeyGroupStrID))

	// Check or valdiate SSH Key Group exists
	skg, err := common.GetSSHKeyGroupFromIDString(ctx, nil, sshKeyGroupStrID, dskgh.dbSession, nil)
	if err != nil {
		if err == common.ErrInvalidID {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to delete SSH Key Group, Invalid SSH Key ID: %s", sshKeyGroupStrID), nil)
		}

		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Failed to delete SSH Key Group, Could not find SSH Key Group with ID: %s ", sshKeyGroupStrID), nil)
		}

		logger.Warn().Err(err).Str("SSH Key Group ID", sshKeyGroupStrID).Msg("error retrieving SSH Key Group from DB by ID")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to delete SSH Key Group, Could not find SSH Key Group with ID: %s, DB error", sshKeyGroupStrID), nil)
	}

	// Check that the SSH Key Group belongs to the Tenant
	if skg.TenantID != tenant.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "SSH Key Group does not belong to current Tenant", nil)
	}

	skgDAO := cdbm.NewSSHKeyGroupDAO(dskgh.dbSession)
	sdDAO := cdbm.NewStatusDetailDAO(dskgh.dbSession)
	skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(dskgh.dbSession)
	skaDAO := cdbm.NewSSHKeyAssociationDAO(dskgh.dbSession)

	var skgsasToSync []cdbm.SSHKeyGroupSiteAssociation

	err = cdb.WithTx(ctx, dskgh.dbSession, func(tx *cdb.Tx) error {
		// acquire an advisory lock on the SSH Key Group on which there could be contention
		// this lock is released when the transaction commits or rollsback
		derr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(skg.ID.String()), nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("Failed to acquire advisory lock on SSH Key Group")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete SSH Key Group, could not acquire data store lock on Group", nil)
		}

		// Update SSH Key Group to set status to Deleting
		if _, derr := skgDAO.Update(
			ctx,
			tx,
			cdbm.SSHKeyGroupUpdateInput{
				SSHKeyGroupID: skg.ID,
				Status:        cutil.GetPtr(cdbm.SSHKeyGroupStatusDeleting),
			},
		); derr != nil {
			logger.Error().Err(derr).Msg("error updating SSH Key Group in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete SSH Key Groups", nil)
		}

		// create a status detail record for the SSH Key Group
		_, derr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: skg.ID.String(), Status: cdbm.SSHKeyGroupStatusDeleting, Message: cutil.GetPtr("received request for deletion, pending processing")})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for SSH Key Group", nil)
		}

		skgsasToSync, _, derr = skgsaDAO.GetAll(ctx, tx, cdbm.SSHKeyGroupSiteAssociationFilterInput{SSHKeyGroupIDs: []uuid.UUID{skg.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving SSH Key Group Associations from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve SSH Key Group Site associations from DB", nil)
		}

		// Update Status Deleting for SSH Key Group Association
		for _, skgsa := range skgsasToSync {
			if skgsa.Status != cdbm.SSHKeyGroupSiteAssociationStatusDeleting {
				// Update SSH Key Group Association to set status to Deleting
				_, serr := skgsaDAO.Update(ctx, tx, cdbm.SSHKeyGroupSiteAssociationUpdateInput{ID: skgsa.ID, Status: cutil.GetPtr(cdbm.SSHKeyGroupSiteAssociationStatusDeleting)})
				if serr != nil {
					logger.Error().Err(serr).Msg("error updating SSH Key Group Association in DB")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete SSH Key Groups", nil)
				}

				// create a status detail record for the SSH Key Group Association
				_, serr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: skgsa.ID.String(), Status: cdbm.SSHKeyGroupSiteAssociationStatusDeleting, Message: cutil.GetPtr("received request for deletion, pending processing")})
				if serr != nil {
					logger.Error().Err(serr).Msg("error creating Status Detail DB entry")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for SSH Key Group Association", nil)
				}
			}
		}

		// IF there are no Sites to sync, then delete it immediately
		if len(skgsasToSync) == 0 {
			// Delete SSH Key Group
			serr := skgDAO.Delete(ctx, tx, skg.ID)
			if serr != nil {
				logger.Error().Err(serr).Msg("error deleting SSH Key Group from DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete SSH Key Group, DB error", nil)
			}

			// Delete SSH Key Associations for SSH Key Group
			skas, _, serr := skaDAO.GetAll(ctx, tx, cdbm.SSHKeyAssociationFilterInput{SSHKeyGroupIDs: []uuid.UUID{skg.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
			if serr != nil {
				logger.Error().Err(serr).Msg("error retrieving SSH Key Associations from DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve SSH Key Associations from DB", nil)
			}
			for _, ska := range skas {
				serr := skaDAO.Delete(ctx, tx, ska.ID)
				if serr != nil {
					logger.Error().Err(serr).Msg("error deleting SSH Key Association from DB")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete SSH Key Association, DB error", nil)
				}
			}
		}

		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to delete SSH Key Group, DB transaction error")
	}

	// Trigger DeleteSSHKeyGroup workflow for each sshkeygroup
	for _, skgsa := range skgsasToSync {
		// Trigger workflow to sync SSH Key Group with Site
		wid, err := sshKeyGroupWorkflow.ExecuteDeleteSSHKeyGroupWorkflow(ctx, dskgh.tc, skgsa.SiteID, skgsa.SSHKeyGroupID)
		if err != nil {
			// Log error but continue, unsynced groups will be re-triggered by inventory
			logger.Error().Err(err).Msg("failed to execute sync SSH Key Group workflow")
			continue
		}

		logger.Info().Str("Workflow ID", *wid).Str("Site ID", skgsa.SiteID.String()).Msg("triggered SSH Key Group sync workflow")
	}

	// Return response
	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusAccepted, model.NewAPIDeletionAcceptedResponse())
}
