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
	"golang.org/x/crypto/ssh"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"

	"github.com/labstack/echo/v4"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
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

// CreateSSHKeyHandler is the API Handler for creating new SSHKey
type CreateSSHKeyHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewCreateSSHKeyHandler initializes and returns a new handler for creating SSH Key
func NewCreateSSHKeyHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) CreateSSHKeyHandler {
	return CreateSSHKeyHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Create an SSH Key
// @Description Create an SSH Key for the org.
// @Tags SSHKey
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APISSHKeyCreateRequest true "SSH Key create request"
// @Success 201 {object} model.APISSHKey
// @Router /v2/org/{org}/nico/sshkey [post]
func (cskh CreateSSHKeyHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("SSHKey", "Create", c)
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

	// Validate the tenant for which this SSH Key is being created
	tenant, err := common.GetTenantForOrg(ctx, nil, cskh.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			logger.Warn().Err(err).Msg("Org does not have a Tenant associated")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("unable to retrieve tenant for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve tenant for org", nil)
	}

	// Validate role, only Tenant Admins are allowed to create SSH Keys
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Validate request
	// Bind request data to API model
	apiRequest := model.APISSHKeyCreateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating SSH Key creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating SSH Key creation request data", verr)
	}

	cotel.SetAttribute(handlerSpan, attribute.String("name", apiRequest.Name))

	// check for name uniqueness for the tenant, ie, tenant cannot have another SSH Key with same name at the site
	skDAO := cdbm.NewSSHKeyDAO(cskh.dbSession)
	sks, tot, err := skDAO.GetAll(
		ctx,
		nil,
		cdbm.SSHKeyFilterInput{
			Names:     []string{apiRequest.Name},
			TenantIDs: []uuid.UUID{tenant.ID},
		},
		cdbp.PageInput{},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("db error checking for name uniqueness of tenant SSH Key")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create SSH Key due to DB error", nil)
	}
	if tot > 0 {
		logger.Warn().Str("tenantId", tenant.ID.String()).Str("name", apiRequest.Name).Msg("SSH Key with same name already exists for Tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "An SSH Key with specified name already exists for Tenant", validation.Errors{
			"id": errors.New(sks[0].ID.String()),
		})
	}

	// Verify SSH Key Group ID
	var dbskg *cdbm.SSHKeyGroup

	if apiRequest.SSHKeyGroupID != nil {
		skgID := *apiRequest.SSHKeyGroupID
		cotel.SetAttribute(handlerSpan, attribute.String("sshKeyGroupID", skgID))

		var serr error
		dbskg, serr = common.GetSSHKeyGroupFromIDString(ctx, nil, skgID, cskh.dbSession, nil)
		if serr != nil {
			if serr == common.ErrInvalidID {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create SSH Key, Invalid SSH Key Group ID: %s", skgID), nil)
			}
			if serr == cdb.ErrDoesNotExist {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create SSH Key, Could not find SSH Key Group with ID: %s ", skgID), nil)
			}
			logger.Warn().Err(serr).Str("SSH Key Group ID", *apiRequest.SSHKeyGroupID).Msg("error retrieving SSH Key Group from DB by ID")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create SSH Key Group, Could not find SSH Key Group with ID: %s due to data store error", skgID), nil)
		}

		if dbskg.TenantID != tenant.ID {
			logger.Warn().Str("Tenant ID", tenant.ID.String()).Str("SSH Key Group ID", *apiRequest.SSHKeyGroupID).Msg("SSH Key Group does not belong to current Tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create SSH Key, SSH Key Group with ID: %s does not belong to Tenant", skgID), nil)
		}

		if dbskg.Status == cdbm.SSHKeyGroupStatusDeleting {
			logger.Warn().Str("SSH Key Group ID", *apiRequest.SSHKeyGroupID).Msg("SSH Key Group is in Deleting state")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create SSH Key, SSH Key Group with ID: %s is in Deleting state", skgID), nil)
		}
	}

	// Validate the SSH Key, and generate the fingerprint
	publicKey, _, _, _, err := ssh.ParseAuthorizedKey([]byte(apiRequest.PublicKey))
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing public key")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Unable to parse public key in api request", nil)
	}
	fingerprint := ssh.FingerprintSHA256(publicKey)
	// the fingerprint is prefixed by "SHA256:", remove that prefix
	fingerprint = fingerprint[7:]

	// Check for uniqueness of fingerprint
	sks, tot, err = skDAO.GetAll(
		ctx,
		nil,
		cdbm.SSHKeyFilterInput{
			TenantIDs:    []uuid.UUID{tenant.ID},
			Fingerprints: []string{fingerprint},
		},
		cdbp.PageInput{},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("db error checking for name uniqueness of tenant SSHKey")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "SSH Key create failed due to data store error", nil)
	}
	if tot > 0 {
		logger.Warn().Str("tenantId", tenant.ID.String()).Msg("SSH Key with same fingerprint already exists for tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "An SSH Key with same fingerprint already exists for Tenant", validation.Errors{
			"id": errors.New(sks[0].ID.String()),
		})
	}

	var dbsk *cdbm.SSHKey
	var skgsas []cdbm.SSHKeyGroupSiteAssociation

	err = cdb.WithTx(ctx, cskh.dbSession, func(tx *cdb.Tx) error {
		// create the ssh key
		// NOTE: Remove `expires` from DB model
		var derr error
		dbsk, derr = skDAO.Create(
			ctx,
			tx,
			cdbm.SSHKeyCreateInput{
				Name:        apiRequest.Name,
				TenantOrg:   org,
				TenantID:    tenant.ID,
				PublicKey:   apiRequest.PublicKey,
				Fingerprint: &fingerprint,
				CreatedBy:   dbUser.ID,
			},
		)
		if derr != nil {
			logger.Error().Err(derr).Msg("unable to create the SSH Key record in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Error creating SSH Key due to data store error", nil)
		}

		if dbskg != nil {
			// Create SSH Key Association for SSH Key Group
			skgDAO := cdbm.NewSSHKeyGroupDAO(cskh.dbSession)

			// Acquire an advisory lock on the SSH Key Group on which there could be contention
			// this lock is released when the transaction commits or rollsback
			derr = tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(dbskg.ID.String()), nil)
			if derr != nil {
				logger.Error().Err(derr).Msg("Failed to acquire advisory lock on sshkey group")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to associate SSH Key with SSH Key Group, could not acquire data store lock on Group", nil)
			}

			skaDAO := cdbm.NewSSHKeyAssociationDAO(cskh.dbSession)
			_, derr = skaDAO.Create(ctx, tx, cdbm.SSHKeyAssociationCreateInput{
				SSHKeyID:      dbsk.ID,
				SSHKeyGroupID: dbskg.ID,
				CreatedBy:     dbUser.ID,
			})
			if derr != nil {
				logger.Error().Err(derr).Msg("unable to create the SSH Key Association record in DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to associate SSH Key with SSH Key Group due to data store error", nil)
			}

			// Calculate and set new versions
			_, derr = skgDAO.GenerateAndUpdateVersion(ctx, tx, dbskg.ID)
			if derr != nil {
				logger.Error().Err(derr).Msg("error updating current version for SSH Key Group")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to set updated version for SSH Key Group", nil)
			}

			skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(cskh.dbSession)
			skgsas, _, derr = skgsaDAO.GetAll(ctx, tx, cdbm.SSHKeyGroupSiteAssociationFilterInput{SSHKeyGroupIDs: []uuid.UUID{dbskg.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
			if derr != nil {
				logger.Error().Err(derr).Msg("error retrieving SSH Key Group Association from DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve SSH Key Group Association from DB", nil)
			}

			// Only mark the group as Syncing if it actually has a site to
			// sync with -- otherwise there's nothing for the sync workflow
			// to do, and the group would get stuck in Syncing forever.
			if len(skgsas) > 0 {
				// Update SSH Key Group status to Syncing
				_, derr = skgDAO.Update(
					ctx,
					tx,
					cdbm.SSHKeyGroupUpdateInput{
						SSHKeyGroupID: dbskg.ID,
						Status:        cutil.GetPtr(cdbm.SSHKeyGroupStatusSyncing),
					},
				)
				if derr != nil {
					logger.Error().Err(derr).Msg("error updating SSH Key Group in DB")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update SSH Key Group status", nil)
				}

				// Create a status detail record for the SSH Key Group
				sdDAO := cdbm.NewStatusDetailDAO(cskh.dbSession)
				_, derr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: dbskg.ID.String(), Status: cdbm.SSHKeyGroupStatusSyncing, Message: cutil.GetPtr("Sync required due to SSH Key creation, pending processing")})
				if derr != nil {
					logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for SSH Key Group", nil)
				}
			}
		}
		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to create SSH Key due to DB transaction error")
	}

	// If SSH Key Group was specified then trigger SyncSSHKeyGroup workflow
	for _, skgsa := range skgsas {
		if skgsa.Status == cdbm.SSHKeyGroupSiteAssociationStatusDeleting {
			continue
		}

		// Trigger workflow to sync SSH Key Group with Site
		wid, err := sshKeyGroupWorkflow.ExecuteSyncSSHKeyGroupWorkflow(ctx, cskh.tc, skgsa.SiteID, skgsa.SSHKeyGroupID, *skgsa.Version)
		if err != nil {
			// Log error but continue, unsynced groups will be triggered by inventory
			logger.Error().Err(err).Msg("failed to execute sync SSH Key Group workflow")
			continue
		}

		logger.Info().Str("Workflow ID", *wid).Str("Site ID", skgsa.SiteID.String()).Msg("triggered SSH Key Group sync workflow")
	}

	// Create response
	apisk := model.NewAPISSHKey(dbsk, []cdbm.SSHKeyAssociation{})
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusCreated, apisk)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateSSHKeyHandler is the API Handler for updating a SSH Key
type UpdateSSHKeyHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewUpdateSSHKeyHandler initializes and returns a new handler for updating SSH Key
func NewUpdateSSHKeyHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) UpdateSSHKeyHandler {
	return UpdateSSHKeyHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Update an existing SSH Key
// @Description Update an existing SSH Key for the org
// @Tags SSHKey
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of SSHKey"
// @Param message body model.APISSHKeyUpdateRequest true "SSH Key update request"
// @Success 200 {object} model.APISSHKey
// @Router /v2/org/{org}/nico/sshkey/{id} [patch]
func (uskh UpdateSSHKeyHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("SSHKey", "Update", c)
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

	// Validate role, only Tenant Admins are allowed to update SSHKey
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get SSH Key ID from URL param
	sshKeyStrID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("sshkey_id", sshKeyStrID))

	sshKeyID, err := uuid.Parse(sshKeyStrID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid SSH Key ID in URL", nil)
	}

	skDAO := cdbm.NewSSHKeyDAO(uskh.dbSession)

	// Validate request
	// Bind request data to API model
	apiRequest := model.APISSHKeyUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating SSH Key update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating SSH Key update data", verr)
	}

	// Check that SSH Key exists
	sk, err := skDAO.GetByID(ctx, nil, sshKeyID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("SSH Key DB entity not found")
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find SSH Key to update", nil)
		}
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Could not find SSH Key to update due to data store error", nil)
	}

	// Get Tenant for this org
	tnDAO := cdbm.NewTenantDAO(uskh.dbSession)

	tenants, _, err := tnDAO.GetAll(ctx, nil, cdbm.TenantFilterInput{Orgs: []string{org}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant for org", nil)
	}

	if len(tenants) == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have a Tenant associated", nil)
	}
	tenant := tenants[0]

	// Check that SSH Key belongs to the Tenant
	if sk.TenantID != tenant.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "SSH Key does not belong to current Tenant", nil)
	}

	// check for name uniqueness for the tenant, ie, tenant cannot have another SSH Key with same name
	if apiRequest.Name != nil && *apiRequest.Name != sk.Name {
		sks, tot, serr := skDAO.GetAll(
			ctx,
			nil,
			cdbm.SSHKeyFilterInput{
				Names:     []string{*apiRequest.Name},
				TenantIDs: []uuid.UUID{tenant.ID},
			},
			cdbp.PageInput{},
			nil,
		)
		if serr != nil {
			logger.Error().Err(serr).Msg("db error checking for name uniqueness of tenant SSH Key")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to update SSH Key due to data store error", nil)
		}
		if tot > 0 {
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Another SSH Key with specified name already exists for Tenant", validation.Errors{
				"id": errors.New(sks[0].ID.String()),
			})
		}
	}

	skaDAO := cdbm.NewSSHKeyAssociationDAO(uskh.dbSession)
	var skas []cdbm.SSHKeyAssociation

	err = cdb.WithTx(ctx, uskh.dbSession, func(tx *cdb.Tx) error {
		// Update SSHKey
		var derr error
		sk, derr = skDAO.Update(
			ctx,
			tx,
			cdbm.SSHKeyUpdateInput{
				SSHKeyID: sk.ID,
				Name:     apiRequest.Name,
			},
		)
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating SSH Key")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update SSH Key due to data store error", nil)
		}

		skas, _, derr = skaDAO.GetAll(ctx, tx, cdbm.SSHKeyAssociationFilterInput{SSHKeyIDs: []uuid.UUID{sk.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving SSH Key association from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve SSH Key Association from DB", nil)
		}
		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to update SSH Key due to DB transaction error")
	}

	// Create response
	apiSSHKey := model.NewAPISSHKey(sk, skas)

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiSSHKey)
}

// ~~~~~ Get Handler ~~~~~ //

// GetSSHKeyHandler is the API Handler for getting an SSH Key
type GetSSHKeyHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetSSHKeyHandler initializes and returns a new handler for getting SSH Key
func NewGetSSHKeyHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetSSHKeyHandler {
	return GetSSHKeyHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get an SSH Key
// @Description Get an SSH Key for the org
// @Tags SSHKey
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of SSH Key"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Tenant'"
// @Success 200 {object} model.APISSHKey
// @Router /v2/org/{org}/nico/sshkey/{id} [get]
func (gskh GetSSHKeyHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("SSHKey", "Get", c)
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

	// Validate role, only Tenant Admins are allowed to update SSH Key
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get  ID from URL param
	sshKeyStrID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("sshkey_id", sshKeyStrID))

	sshKeyID, err := uuid.Parse(sshKeyStrID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid SSH Key ID in URL", nil)
	}

	skDAO := cdbm.NewSSHKeyDAO(gskh.dbSession)

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.SSHKeyRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Check that SSH Key exists
	sk, err := skDAO.GetByID(ctx, nil, sshKeyID, qIncludeRelations)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("SSH Key DB entity not found")
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find SSH Key", nil)
		}
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Could not find SSH Key due to data store error", nil)
	}

	// Get Tenant for this org
	tnDAO := cdbm.NewTenantDAO(gskh.dbSession)

	tenants, _, err := tnDAO.GetAll(ctx, nil, cdbm.TenantFilterInput{Orgs: []string{org}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant for org", nil)
	}

	if len(tenants) == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have a Tenant associated", nil)
	}
	tenant := tenants[0]

	// Check that SSH Key belongs to the Tenant
	if sk.TenantID != tenant.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "SSH Key does not belong to current Tenant", nil)
	}

	skaDAO := cdbm.NewSSHKeyAssociationDAO(gskh.dbSession)
	skas, _, err := skaDAO.GetAll(ctx, nil, cdbm.SSHKeyAssociationFilterInput{SSHKeyIDs: []uuid.UUID{sk.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving SSH Key association from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SSH Key Association from DB", nil)
	}

	// Create response
	apiSSHKey := model.NewAPISSHKey(sk, skas)

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiSSHKey)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllSSHKeyHandler is the API Handler for retrieving all SSH Keys
type GetAllSSHKeyHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetAllSSHKeyHandler initializes and returns a new handler for retreiving all SSH Keys
func NewGetAllSSHKeyHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllSSHKeyHandler {
	return GetAllSSHKeyHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get all SSH Keys
// @Description Get all SSH Keys for the org
// @Tags SSHKey
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param sshKeyGroupId query string true "ID of SSH Key Group"
// @Param query query string false "Query input for full text search"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Tenant'"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {array} []model.APISSHKey
// @Router /v2/org/{org}/nico/sshkey [get]
func (gaskh GetAllSSHKeyHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("SSHKey", "GetAll", c)
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

	// Validate role, only Tenant Admins are allowed to retrieve
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
	err = pageRequest.Validate(cdbm.SSHKeyOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest,
			"Failed to validate pagination request data", err)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.SSHKeyRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Get Tenant for this org
	tnDAO := cdbm.NewTenantDAO(gaskh.dbSession)

	tenants, _, err := tnDAO.GetAll(ctx, nil, cdbm.TenantFilterInput{Orgs: []string{org}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant for org", nil)
	}

	if len(tenants) == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have a Tenant associated", nil)
	}
	tenant := tenants[0]

	// verify sshKeyGroupId if specified in query string
	qSshKeyGroupID := c.QueryParam("sshKeyGroupId")
	var sshKeyGroupIDs []uuid.UUID
	if qSshKeyGroupID != "" {
		sshKeyGroup, err := common.GetSSHKeyGroupFromIDString(ctx, nil, qSshKeyGroupID, gaskh.dbSession, nil)
		if err != nil {
			logger.Warn().Err(err).Msg("error getting SSH Key Group in request")
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find SSH Key Group specified in request", nil)
		}
		if sshKeyGroup.TenantID != tenant.ID {
			logger.Warn().Msg("tenant in SSH Key Group does not belong to tenant in org")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant for SSH Key Group in request does not match tenant in org", nil)
		}
		sshKeyGroupIDs = append(sshKeyGroupIDs, sshKeyGroup.ID)
	}

	// Get query text for full text search from query param
	searchQuery := common.GetSearchQuery(c)
	if searchQuery != nil {
		cotel.SetAttribute(handlerSpan, attribute.String("query", *searchQuery))
	}

	// Get all SSH Keys by Tenant
	skDAO := cdbm.NewSSHKeyDAO(gaskh.dbSession)
	dbSSHKeys, total, serr := skDAO.GetAll(
		ctx,
		nil,
		cdbm.SSHKeyFilterInput{
			TenantIDs:      []uuid.UUID{tenant.ID},
			SSHKeyGroupIDs: sshKeyGroupIDs,
			SearchQuery:    searchQuery,
		},
		cdbp.PageInput{
			Offset:  pageRequest.Offset,
			Limit:   pageRequest.Limit,
			OrderBy: pageRequest.OrderBy,
		},
		qIncludeRelations,
	)
	if serr != nil {
		logger.Error().Err(serr).Msg("error retrieving SSHKeys for tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SSHKeys due to datastore error", nil)
	}

	// Create response
	skaDAO := cdbm.NewSSHKeyAssociationDAO(gaskh.dbSession)
	apiSSHKeys := []model.APISSHKey{}

	for _, sk := range dbSSHKeys {
		skas, _, err := skaDAO.GetAll(ctx, nil, cdbm.SSHKeyAssociationFilterInput{SSHKeyIDs: []uuid.UUID{sk.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error getting SSH Key association records")
		}

		// Create response
		dbSSHKey := sk
		apiSSHKey := model.NewAPISSHKey(&dbSSHKey, skas)
		apiSSHKeys = append(apiSSHKeys, *apiSSHKey)
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

	return c.JSON(http.StatusOK, apiSSHKeys)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteSSHKeyHandler is the API Handler for deleting an SSH Key
type DeleteSSHKeyHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewDeleteSSHKeyHandler initializes and returns a new handler for deleting a SSH Key
func NewDeleteSSHKeyHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) DeleteSSHKeyHandler {
	return DeleteSSHKeyHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Delete an SSH Key
// @Description Delete an SSH Key from the org
// @Tags SSHKey
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of SSHKey"
// @Success 202
// @Router /v2/org/{org}/nico/sshkey/{id} [delete]
func (dskh DeleteSSHKeyHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("SSHKey", "Delete", c)
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

	// Validate role, only Tenant Admins are allowed to delete SSHKeys
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get Tenant for this org
	tnDAO := cdbm.NewTenantDAO(dskh.dbSession)

	tenants, _, err := tnDAO.GetAll(ctx, nil, cdbm.TenantFilterInput{Orgs: []string{org}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant for org", nil)
	}

	if len(tenants) == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have a Tenant associated with it", nil)
	}
	tenant := tenants[0]

	// Get ID from URL param
	sshKeyStrID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("sshkey_id", sshKeyStrID))

	sshKeyID, err := uuid.Parse(sshKeyStrID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid SSH Key ID in URL", nil)
	}

	// Get SSH Key
	skDAO := cdbm.NewSSHKeyDAO(dskh.dbSession)

	sk, err := skDAO.GetByID(ctx, nil, sshKeyID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find SSH Key with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving SSH Key from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SSH Key due to datastore error", nil)
	}

	// Check that the SSH Key belongs to the Tenant
	if sk.TenantID != tenant.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "SSHKey does not belong to current Tenant", nil)
	}

	skaDAO := cdbm.NewSSHKeyAssociationDAO(dskh.dbSession)
	skgDAO := cdbm.NewSSHKeyGroupDAO(dskh.dbSession)
	sdDAO := cdbm.NewStatusDetailDAO(dskh.dbSession)
	skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(dskh.dbSession)

	var skgsasToSync []cdbm.SSHKeyGroupSiteAssociation

	err = cdb.WithTx(ctx, dskh.dbSession, func(tx *cdb.Tx) error {
		skas, _, derr := skaDAO.GetAll(ctx, tx, cdbm.SSHKeyAssociationFilterInput{SSHKeyIDs: []uuid.UUID{sk.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, []string{cdbm.SSHKeyGroupRelationName})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving SSH Key association from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve SSH Key Association from DB", nil)
		}

		// Delete all SSH Key Associations
		skgIDs := []uuid.UUID{}
		for _, ska := range skas {
			// acquire an advisory lock on the SSH Key Group on which there could be contention
			// this lock is released when the transaction commits or rollsback
			derr = tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(ska.SSHKeyGroupID.String()), nil)
			if derr != nil {
				logger.Error().Err(derr).Msg("Failed to acquire advisory lock on sshkey group")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update dissociate SSH Key from one or more SSH Key Groups, could not acquire data store lock on Group", nil)
			}

			// Delete Key Association
			derr = skaDAO.Delete(ctx, tx, ska.ID)
			if derr != nil {
				logger.Error().Err(derr).Msg("unable to delete SSH Key Association record in DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete SSH Key Association due to data store error", nil)
			}

			// Only allow to update SSHKeyGroup if it is not in Deleting State
			if ska.SSHKeyGroup != nil && ska.SSHKeyGroup.Status == cdbm.SSHKeyGroupStatusDeleting {
				continue
			}

			// Calculate and set new versions
			_, derr = skgDAO.GenerateAndUpdateVersion(ctx, tx, ska.SSHKeyGroupID)
			if derr != nil {
				logger.Error().Err(derr).Msg("error calculating current version for SSH Key Group")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to calculate current version for SSH Key Group", nil)
			}

			skgIDs = append(skgIDs, ska.SSHKeyGroupID)
		}

		// Delete SSH Key in DB
		derr = skDAO.Delete(ctx, tx, sk.ID)
		if derr != nil {
			logger.Error().Err(derr).Msg("error deleting SSHKey in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete SSH Key due to data store error", nil)
		}

		skgsasToSync, _, derr = skgsaDAO.GetAll(ctx, tx, cdbm.SSHKeyGroupSiteAssociationFilterInput{SSHKeyGroupIDs: skgIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, []string{cdbm.SSHKeyGroupRelationName})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving SSH Key Group Associations related to SSH Key from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve SSH Key Group Associations from DB", nil)
		}

		// Update group-level status once per unique SSH Key Group. Without
		// this dedup, groups attached to multiple sites would have the same
		// status row written once per site, bloating status history.
		seenGroups := make(map[uuid.UUID]struct{}, len(skgsasToSync))
		for _, skgsa := range skgsasToSync {
			if _, seen := seenGroups[skgsa.SSHKeyGroupID]; seen {
				continue
			}
			seenGroups[skgsa.SSHKeyGroupID] = struct{}{}

			// Update SSH Key Group version and status to Syncing
			_, derr = skgDAO.Update(
				ctx,
				tx,
				cdbm.SSHKeyGroupUpdateInput{
					SSHKeyGroupID: skgsa.SSHKeyGroupID,
					Status:        cutil.GetPtr(cdbm.SSHKeyGroupStatusSyncing),
				},
			)
			if derr != nil {
				logger.Error().Err(derr).Msg("error updating SSH Key Group in DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update SSH Key Group status", nil)
			}

			// Create a status detail record for the SSH Key Group
			_, derr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: skgsa.SSHKeyGroupID.String(), Status: cdbm.SSHKeyGroupStatusSyncing, Message: cutil.GetPtr("Sync required due to SSH Key deletion, pending processing")})
			if derr != nil {
				logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for SSH Key Group", nil)
			}
		}
		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to delete SSH Key due to DB transaction error")
	}

	// Trigger SyncSSHKeyGroup workflow for each SSH Key Group
	for _, skgsa := range skgsasToSync {
		// Skip triggering SSHKeyGroup sync if it is in Deleting state
		if skgsa.SSHKeyGroup != nil && skgsa.SSHKeyGroup.Status == cdbm.SSHKeyGroupStatusDeleting {
			continue
		}

		if skgsa.Status == cdbm.SSHKeyGroupSiteAssociationStatusDeleting {
			continue
		}

		// Trigger workflow to sync SSH Key Group with Site
		wid, err := sshKeyGroupWorkflow.ExecuteSyncSSHKeyGroupWorkflow(ctx, dskh.tc, skgsa.SiteID, skgsa.SSHKeyGroupID, *skgsa.Version)
		if err != nil {
			// Log error but continue, unsynced groups will be triggered by inventory
			logger.Error().Err(err).Msg("failed to execute sync SSH Key Group workflow")
			continue
		}

		logger.Info().Str("Workflow ID", *wid).Str("Site ID", skgsa.SiteID.String()).Msg("triggered SSH Key Group sync workflow")
	}

	// Return response
	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusAccepted, model.NewAPIDeletionAcceptedResponse())
}
