// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"slices"

	"go.opentelemetry.io/otel/attribute"
	temporalClient "go.temporal.io/sdk/client"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/proto"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"

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
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
)

// NICo Core (forge.Forge) Operating System methods proxied for iPXE / Templated
// iPXE Operating Systems via the generic Core gRPC proxy (epic #1927). Image-type
// Operating Systems continue to use the dedicated OsImage site workflows.
const (
	createOperatingSystemMethod = "/forge.Forge/CreateOperatingSystem"
	updateOperatingSystemMethod = "/forge.Forge/UpdateOperatingSystem"
	deleteOperatingSystemMethod = "/forge.Forge/DeleteOperatingSystem"
)

// syncOperatingSystemToSitesViaProxy pushes an iPXE / Templated iPXE Operating
// System create or update to each associated site through the generic NICo Core
// gRPC proxy, updating each site association's status (Synced on success, Error
// on failure). The same request proto is sent to every site (the OS definition is
// site-independent). It returns the number of sites that failed to sync.
func syncOperatingSystemToSitesViaProxy(
	ctx context.Context,
	logger zerolog.Logger,
	dbSession *cdb.Session,
	scp *sc.ClientPool,
	ossas []cdbm.OperatingSystemSiteAssociation,
	fullMethod string,
	req proto.Message,
) int {
	siteErrors := 0
	for _, ossa := range ossas {
		slogger := logger.With().Str("Site ID", ossa.SiteID.String()).Logger()

		stc, cerr := scp.GetClientByID(ossa.SiteID)
		if cerr != nil {
			slogger.Error().Err(cerr).Msg("failed to retrieve Temporal client for Site")
			// Site is already counted as an error below; a bookkeeping failure is
			// logged inside and does not change the outcome for this site.
			_ = updateOSSAStatusViaProxy(ctx, slogger, dbSession, ossa.ID, cdbm.OperatingSystemSiteAssociationStatusError, "failed to connect to site")
			siteErrors++
			continue
		}

		// The site ID is the shared key used to encrypt any redacted secret fields
		// for transport; no top-level secret fields are redacted here (artifact
		// authTokens are nested and carried as-is).
		perr := common.ExecuteCoreGRPC(ctx, stc, fullMethod, req, nil, ossa.SiteID.String())
		if perr != nil {
			slogger.Error().Err(perr).Int("code", perr.Code).Msg("failed to sync Operating System to site via Core proxy")
			_ = updateOSSAStatusViaProxy(ctx, slogger, dbSession, ossa.ID, cdbm.OperatingSystemSiteAssociationStatusError, "failed to sync Operating System to site")
			siteErrors++
			continue
		}

		// The proxy call succeeded, but if we cannot durably record the Synced
		// status the association state is unreliable, so treat that as a site
		// error rather than reporting the OS as fully synced.
		if serr := updateOSSAStatusViaProxy(ctx, slogger, dbSession, ossa.ID, cdbm.OperatingSystemSiteAssociationStatusSynced, "Operating System successfully synced to site"); serr != nil {
			siteErrors++
		}
	}
	return siteErrors
}

// updateOSSAStatusViaProxy updates an Operating System Site Association status and
// records the corresponding status detail atomically in a single transaction, so
// the status and its audit entry cannot diverge. Any persistence error is logged
// and returned so callers can account for it (e.g. as a site error when computing
// aggregate status) instead of silently reporting success.
func updateOSSAStatusViaProxy(ctx context.Context, logger zerolog.Logger, dbSession *cdb.Session, ossaID uuid.UUID, status string, message string) error {
	err := cdb.WithTx(ctx, dbSession, func(tx *cdb.Tx) error {
		ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(dbSession)
		sdDAO := cdbm.NewStatusDetailDAO(dbSession)
		if _, uerr := ossaDAO.Update(ctx, tx, cdbm.OperatingSystemSiteAssociationUpdateInput{
			OperatingSystemSiteAssociationID: ossaID,
			Status:                           cutil.GetPtr(status),
		}); uerr != nil {
			return uerr
		}
		if _, cerr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: ossaID.String(), Status: status, Message: &message}); cerr != nil {
			return cerr
		}
		return nil
	})
	if err != nil {
		logger.Error().Err(err).Str("Status", status).Msg("failed to persist Operating System Site Association status")
	}
	return err
}

// aggregateSyncMessage returns the status-detail message for a proxy sync aggregate
// status update (create/update), where the outcome is either a full sync or a
// partial/complete sync failure.
func aggregateSyncMessage(hadErrors bool) string {
	if hadErrors {
		return "failed to sync Operating System to one or more sites"
	}
	return "Operating System successfully synced to all sites"
}

// updateOperatingSystemAggregateStatus sets the Operating System's aggregate status
// after a proxy sync attempt: Ready when all sites synced, Error when one or more failed.
// Callers supply the operation-specific status-detail message (e.g. a sync message for
// create/update, a deletion message for delete). The status update and its status-detail
// audit entry are written together in a single transaction so they cannot diverge, and
// any transaction/DB failure is returned to the caller rather than only logged.
//
// A Deactivated Operating System keeps its Deactivated status: the sync still pushes the
// definition to sites, but the aggregate readiness/error status must not override the
// user-initiated deactivation.
func updateOperatingSystemAggregateStatus(ctx context.Context, logger zerolog.Logger, dbSession *cdb.Session, osID uuid.UUID, hadErrors bool, message string) error {
	osDAO := cdbm.NewOperatingSystemDAO(dbSession)

	status := cdbm.OperatingSystemStatusReady
	if hadErrors {
		status = cdbm.OperatingSystemStatusError
	}

	if err := cdb.WithTx(ctx, dbSession, func(tx *cdb.Tx) error {
		// Serialize with other Operating System state changes (deactivation,
		// deletion) via the per-OS advisory lock, held for the life of this
		// transaction. This closes the read-then-write race: the deactivation guard
		// re-reads the status under the lock, so a concurrent Deactivate cannot land
		// between the read and the write and be clobbered by Ready/Error.
		if lerr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(osID.String()), nil); lerr != nil {
			logger.Error().Err(lerr).Msg("failed to acquire advisory lock on Operating System for aggregate status update")
			return lerr
		}

		existing, gerr := osDAO.GetByID(ctx, tx, osID, nil)
		if gerr != nil {
			logger.Error().Err(gerr).Msg("failed to read Operating System before aggregate status update")
			return gerr
		}
		// A Deactivated Operating System keeps its Deactivated status: the sync still
		// pushed the definition to sites, but the aggregate readiness/error status
		// must not override the user-initiated deactivation.
		if existing.Status == cdbm.OperatingSystemStatusDeactivated {
			logger.Info().Msg("Operating System is deactivated, preserving Deactivated status after site sync")
			return nil
		}

		if _, uerr := osDAO.Update(ctx, tx, cdbm.OperatingSystemUpdateInput{OperatingSystemId: osID, Status: &status}); uerr != nil {
			return uerr
		}
		sdDAO := cdbm.NewStatusDetailDAO(dbSession)
		if _, cerr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: osID.String(), Status: status, Message: &message}); cerr != nil {
			return cerr
		}
		return nil
	}); err != nil {
		logger.Error().Err(err).Msg("failed to persist aggregate Operating System status")
		return err
	}
	return nil
}

// validateIpxeTemplateAvailableAtSites verifies the referenced iPXE template is
// available (has an IpxeTemplateSiteAssociation) at every one of the given Sites.
// A Templated iPXE Operating System is rendered on the Site from its template, so
// per the OS sync contract it can only be synced to a Site whose Site currently has
// that template; creating or updating one for a Site that lacks it would persist a
// definition that can never render there. templateID must be a valid template UUID;
// a non-existent template (no associations anywhere) is reported as unavailable at
// every Site. An empty site set is a no-op.
func validateIpxeTemplateAvailableAtSites(ctx context.Context, dbSession *cdb.Session, logger zerolog.Logger, templateID string, siteIDs []uuid.UUID) *cutil.APIError {
	if len(siteIDs) == 0 {
		return nil
	}

	tid, perr := uuid.Parse(templateID)
	if perr != nil {
		return cutil.NewAPIError(http.StatusBadRequest, "iPXE template ID specified in request is not a valid UUID", nil)
	}

	itsaDAO := cdbm.NewIpxeTemplateSiteAssociationDAO(dbSession)
	itsas, _, err := itsaDAO.GetAll(ctx, nil,
		cdbm.IpxeTemplateSiteAssociationFilterInput{
			IpxeTemplateIDs: []uuid.UUID{tid},
			SiteIDs:         siteIDs,
		},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Str("ipxeTemplateId", templateID).Msg("error retrieving iPXE template Site associations for Operating System validation")
		return cutil.NewAPIError(http.StatusInternalServerError, "Failed to validate iPXE template availability, DB error", nil)
	}

	available := make(map[uuid.UUID]struct{}, len(itsas))
	for _, a := range itsas {
		available[a.SiteID] = struct{}{}
	}
	missing := make([]string, 0)
	for _, sid := range siteIDs {
		if _, ok := available[sid]; !ok {
			missing = append(missing, sid.String())
		}
	}
	if len(missing) > 0 {
		logger.Warn().Str("ipxeTemplateId", templateID).Msg("iPXE template is not available at one or more target Sites")
		return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("iPXE template %s specified in request is not available at Site(s): %v", templateID, missing), nil)
	}
	return nil
}

// getTenantSiteIDs returns the IDs of all sites the tenant has access to,
// regardless of site status. Used to scope provider-owned Operating System
// visibility for tenant admins.
func getTenantSiteIDs(ctx context.Context, dbSession *cdb.Session, tenantID uuid.UUID) ([]uuid.UUID, error) {
	tsDAO := cdbm.NewTenantSiteDAO(dbSession)
	tss, _, err := tsDAO.GetAll(ctx, nil,
		cdbm.TenantSiteFilterInput{TenantIDs: []uuid.UUID{tenantID}},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	if err != nil {
		return nil, err
	}
	ids := make([]uuid.UUID, len(tss))
	for i, ts := range tss {
		ids[i] = ts.SiteID
	}
	return ids, nil
}

// ~~~~~ Create Handler ~~~~~ //

// CreateOperatingSystemHandler is the API Handler for creating new OperatingSystem
type CreateOperatingSystemHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewCreateOperatingSystemHandler initializes and returns a new handler for creating OperatingSystem
func NewCreateOperatingSystemHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) CreateOperatingSystemHandler {
	return CreateOperatingSystemHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Create an OperatingSystem
// @Description Create an OperatingSystem
// @Tags OperatingSystem
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIOperatingSystemCreateRequest true "OperatingSystem creation request"
// @Success 201 {object} model.APIOperatingSystem
// @Router /v2/org/{org}/nico/operating-system [post]
func (csh CreateOperatingSystemHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("OperatingSystem", "Create", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ip, tenant, apiError := common.IsProviderOrTenant(ctx, logger, csh.dbSession, org, dbUser, false, nil)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIOperatingSystemCreateRequest{}
	err := c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Infer OS type from the provided source fields (ipxeScript -> iPXE,
	// ipxeTemplateId -> Templated iPXE, otherwise Image).
	osType := apiRequest.GetOperatingSystemType()

	// Provider Admin is limited to iPXE Template-based OSes. When both roles
	// allow the action, Provider Admin takes priority (provider-owned OS).
	allowedByProvider := ip != nil && osType == cdbm.OperatingSystemTypeTemplatedIPXE
	allowedByTenant := tenant != nil
	if !allowedByProvider && !allowedByTenant {
		logger.Warn().Msg("caller is not permitted to create this Operating System")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Provider Admin can only create iPXE Template-based Operating Systems", nil)
	}

	// If the caller provided an explicit tenantId in the body (deprecated), validate
	// it matches the org's tenant.
	// TODO: tenantId as parameter is deprecated and will need to be removed by 2026-10-01.
	if tenant != nil && apiRequest.TenantID != nil && *apiRequest.TenantID != tenant.ID.String() {
		logger.Warn().Str("tenantId", *apiRequest.TenantID).Msg("TenantID in request does not match org's Tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "TenantID specified in request does not match org's Tenant", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Operating System creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Operating System request creation data", verr)
	}

	// Validate and Set UserData
	verr = apiRequest.ValidateAndSetUserData(csh.cfg.GetSitePhoneHomeUrl())
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating user data in Operating System creation request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating user data in Operating System creation request", verr)
	}

	// Check for name uniqueness within the owner's scope (provider or tenant).
	// TODO consider doing this with an advisory lock for correctness
	osDAO := cdbm.NewOperatingSystemDAO(csh.dbSession)
	uniquenessFilter := cdbm.OperatingSystemFilterInput{Names: []string{apiRequest.Name}}
	if allowedByProvider {
		uniquenessFilter.InfrastructureProviderID = &ip.ID
	} else {
		uniquenessFilter.TenantIDs = []uuid.UUID{tenant.ID}
	}
	oss, tot, err := osDAO.GetAll(ctx, nil, uniquenessFilter, cdbp.PageInput{}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("db error checking for name uniqueness of os")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create OperatingSystem due to DB error", nil)
	}
	if tot > 0 {
		logger.Warn().Str("name", apiRequest.Name).Msg("Operating System with same name already exists")
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Another Operating System with specified name already exists", validation.Errors{
			"id": errors.New(oss[0].ID.String()),
		})
	}

	// Set the phoneHomeEnabled if provided in request
	phoneHomeEnabled := false
	if apiRequest.PhoneHomeEnabled != nil {
		phoneHomeEnabled = *apiRequest.PhoneHomeEnabled
	}

	// Resolve and validate target sites. Site ownership differs by caller:
	// provider-owned OSes (Templated iPXE only) target the provider's own sites;
	// tenant-owned OSes target sites the tenant has access to.
	tsDAO := cdbm.NewTenantSiteDAO(csh.dbSession)
	rdbst := []cdbm.Site{}
	sttsmap := map[uuid.UUID]*cdbm.TenantSite{}
	dbossd := []cdbm.StatusDetail{}

	requestedSiteIDs := make([]uuid.UUID, 0, len(apiRequest.SiteIDs))
	for _, siteID := range apiRequest.SiteIDs {
		parsedSiteID, perr := uuid.Parse(siteID)
		if perr != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create Operating System, invalid Site ID: %s", siteID), nil)
		}
		requestedSiteIDs = append(requestedSiteIDs, parsedSiteID)
	}

	sitesByID := make(map[uuid.UUID]*cdbm.Site, len(requestedSiteIDs))
	if len(requestedSiteIDs) > 0 {
		siteDAO := cdbm.NewSiteDAO(csh.dbSession)
		sites, _, serr := siteDAO.GetAll(
			ctx,
			nil,
			cdbm.SiteFilterInput{SiteIDs: requestedSiteIDs},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		if serr != nil {
			logger.Error().Err(serr).Msg("error retrieving Sites from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Sites specified in request, DB error", nil)
		}
		for i := range sites {
			site := &sites[i]
			sitesByID[site.ID] = site
		}
		for _, siteID := range requestedSiteIDs {
			if _, ok := sitesByID[siteID]; !ok {
				return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Failed to create Operating System, could not find Site with ID: %s", siteID.String()), nil)
			}
		}
	}

	if !allowedByProvider && len(requestedSiteIDs) > 0 {
		// Tenant-owned: retrieve the requested TenantSite associations in one query.
		tss, _, terr := tsDAO.GetAll(
			ctx,
			nil,
			cdbm.TenantSiteFilterInput{
				TenantIDs: []uuid.UUID{tenant.ID},
				SiteIDs:   requestedSiteIDs,
			},
			cdbp.PageInput{
				Limit: cutil.GetPtr(cdbp.TotalLimit),
			},
			nil,
		)
		if terr != nil {
			logger.Error().Err(terr).Msg("db error retrieving TenantSite records for Tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site associations for Tenant, DB error", nil)
		}
		for _, ts := range tss {
			cts := ts
			sttsmap[ts.SiteID] = &cts
		}
	}

	for _, siteID := range requestedSiteIDs {
		site := sitesByID[siteID]
		if site.Status != cdbm.SiteStatusRegistered {
			logger.Warn().Str("siteId", siteID.String()).Msg("Unable to associate Operating System to Site; Site is not in Registered state")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create Operating System, Site: %s specified in request is not in Registered state", siteID.String()), nil)
		}
		if allowedByProvider {
			if site.InfrastructureProviderID != ip.ID {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Unable to associate Operating System with Site: %s, Site does not belong to provider", siteID.String()), nil)
			}
		} else {
			// Validate the TenantSite exists for current tenant and this site
			if _, ok := sttsmap[siteID]; !ok {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Unable to associate Operating System with Site: %s, Tenant does not have access to Site", siteID.String()), nil)
			}

			// Validate the Site has the ImageBasedOperatingSystem capability enabled for Image based Operating Systems
			if osType == cdbm.OperatingSystemTypeImage && (site.Config == nil || !site.Config.ImageBasedOperatingSystem) {
				logger.Warn().Str("siteId", siteID.String()).Msg("Image based Operating System is not supported for Site, ImageBasedOperatingSystem capability is not enabled")
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Creation of Image based Operating Systems is not supported. Site must have ImageBasedOperatingSystem capability enabled.", nil)
			}
		}
		rdbst = append(rdbst, *site)
	}

	// A Templated iPXE Operating System is rendered on-Site from its iPXE template,
	// so it can only be synced to Sites that currently have the template. Reject up
	// front if the referenced template is unavailable at any target Site rather than
	// persist a definition that can never render there.
	if osType == cdbm.OperatingSystemTypeTemplatedIPXE {
		targetSiteIDs := make([]uuid.UUID, 0, len(rdbst))
		for i := range rdbst {
			targetSiteIDs = append(targetSiteIDs, rdbst[i].ID)
		}
		if apiErr := validateIpxeTemplateAvailableAtSites(ctx, csh.dbSession, logger, *apiRequest.IpxeTemplateId, targetSiteIDs); apiErr != nil {
			return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
		}
	}

	// Create status based on OS type. iPXE / Templated iPXE definitions are pushed
	// to sites after commit, so Templated iPXE starts in Syncing; raw iPXE (no site
	// associations) is immediately Ready.
	osStatus := cdbm.OperatingSystemStatusReady
	osStatusMessage := "Operating System is ready for use"
	if osType == cdbm.OperatingSystemTypeImage || osType == cdbm.OperatingSystemTypeTemplatedIPXE {
		osStatus = cdbm.OperatingSystemStatusSyncing
		osStatusMessage = "received Operating System creation request, syncing"
	}

	// Assign ownership: provider-owned OSes carry InfrastructureProviderID
	// (tenant_id=nil); tenant-owned OSes carry TenantID
	// (infrastructure_provider_id=nil). When pushed to nico-core, tenant-owned
	// OSes carry tenant_organization_id while provider-owned OSes omit it.
	var ownerTenantID *uuid.UUID
	var ownerProviderID *uuid.UUID
	if allowedByProvider {
		ownerProviderID = &ip.ID
	} else {
		ownerTenantID = &tenant.ID
	}

	// Values needed after the transaction closure
	var os *cdbm.OperatingSystem
	var dbossa []cdbm.OperatingSystemSiteAssociation
	// timeoutResp captures any post-rollback work (terminating timed-out
	// Temporal workflows) that must run after the transaction has been rolled
	// back. It is invoked after the closure if non-nil.
	var timeoutResp func() error

	err = cdb.WithTx(ctx, csh.dbSession, func(tx *cdb.Tx) error {
		// Create the db record for Operating System
		osInput := cdbm.OperatingSystemCreateInput{
			Name:                     apiRequest.Name,
			Description:              apiRequest.Description,
			Org:                      org,
			TenantID:                 ownerTenantID,
			InfrastructureProviderID: ownerProviderID,
			OsType:                   osType,
			ImageURL:                 apiRequest.ImageURL,
			ImageSHA:                 apiRequest.ImageSHA,
			ImageAuthType:            apiRequest.ImageAuthType,
			ImageAuthToken:           apiRequest.ImageAuthToken,
			ImageDisk:                apiRequest.ImageDisk,
			RootFsId:                 apiRequest.RootFsID,
			RootFsLabel:              apiRequest.RootFsLabel,
			IpxeScript:               apiRequest.IpxeScript,
			IpxeTemplateId:           apiRequest.IpxeTemplateId,
			IpxeTemplateParameters:   apiRequest.IpxeTemplateParameters.ToDBModel(),
			IpxeTemplateArtifacts:    apiRequest.IpxeTemplateArtifacts.ToDBModel(),
			UserData:                 apiRequest.UserData,
			AllowOverride:            apiRequest.AllowOverride,
			EnableBlockStorage:       apiRequest.EnableBlockStorage,
			PhoneHomeEnabled:         phoneHomeEnabled,
			Status:                   osStatus,
			CreatedBy:                dbUser.ID,
		}
		createdOs, derr := osDAO.Create(ctx, tx, osInput)
		if derr != nil {
			logger.Error().Err(derr).Msg("unable to create Operating System record in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed creating Operating System record", nil)
		}
		os = createdOs

		// Create the status detail record for Operating System
		sdDAO := cdbm.NewStatusDetailDAO(csh.dbSession)
		ossd, derr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: os.ID.String(), Status: *cutil.GetPtr(osStatus), Message: &osStatusMessage})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for Operating System", nil)
		}

		if ossd == nil {
			logger.Error().Msg("Status Detail DB entry not returned from Create")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to get new Status Detail for Operating System", nil)
		}
		dbossd = append(dbossd, *ossd)

		// Create Operating System Site Associations
		ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(csh.dbSession)
		for _, st := range rdbst {
			// Create Operating System Site Association
			ossa, derr := ossaDAO.Create(
				ctx,
				tx,
				cdbm.OperatingSystemSiteAssociationCreateInput{
					OperatingSystemID: os.ID,
					SiteID:            st.ID,
					Status:            cdbm.OperatingSystemSiteAssociationStatusSyncing,
					CreatedBy:         dbUser.ID,
				},
			)
			if derr != nil {
				logger.Error().Err(derr).Msg("unable to create the Operating System association record in DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to associate Operating System with Site, DB error", nil)
			}

			// Create Status details
			_, derr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: ossa.ID.String(), Status: *cutil.GetPtr(cdbm.OperatingSystemSiteAssociationStatusSyncing), Message: cutil.GetPtr("received Operating System Association create request, syncing")})
			if derr != nil {
				logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for Operating System Association", nil)
			}

			// Update Operating System Site Association version
			_, derr = ossaDAO.GenerateAndUpdateVersion(ctx, tx, ossa.ID)
			if derr != nil {
				logger.Error().Err(derr).Msg("error updating version for created Operating System Association")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to set version for created Operating System Association, DB error", nil)
			}
		}

		// Retrieve Operating System Associations details
		retossa, _, derr := ossaDAO.GetAll(
			ctx,
			tx,
			cdbm.OperatingSystemSiteAssociationFilterInput{
				OperatingSystemIDs: []uuid.UUID{os.ID},
			},
			cdbp.PageInput{
				Limit: cutil.GetPtr(cdbp.TotalLimit),
			},
			[]string{cdbm.SiteRelationName, cdbm.OperatingSystemRelationName},
		)
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Operating System Site associations from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Operating System Site associations from DB", nil)
		}
		dbossa = retossa

		// Trigger workflows to sync Image based Operating System with various Sites.
		// iPXE / Templated iPXE definitions are pushed to sites via the Core gRPC
		// proxy after the transaction commits (see below), so they are skipped here.
		for _, ossa := range dbossa {
			if os.Type != cdbm.OperatingSystemTypeImage {
				continue
			}
			// Iteration body wrapped in a function literal so `defer cancel()`
			// scopes to the iteration; otherwise the deferred cancels would
			// pile up until the WithTx closure returns.
			iterErr := func() *cutil.APIError {
				// Get the temporal client for the site we are working with.
				stc, derr := csh.scp.GetClientByID(ossa.SiteID)
				if derr != nil {
					logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
				}

				createOsRequest := apiRequest.ToImageProto(os, tenant.Org)

				workflowOptions := temporalClient.StartWorkflowOptions{
					ID:                       "image-os-create-" + ossa.SiteID.String() + "-" + os.ID.String() + "-" + *ossa.Version,
					WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
					TaskQueue:                queue.SiteTaskQueue,
				}

				logger.Info().Str("Site ID", ossa.SiteID.String()).Msg("triggering Image based Operating System create workflow ")

				// Add context deadlines
				wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
				defer cancel()

				// Trigger Site workflow
				we, wferr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "CreateOsImage", createOsRequest)
				if wferr != nil {
					logger.Error().Err(wferr).Msg("failed to synchronously start Temporal workflow to create Operating System")
					return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed start sync workflow to create Operating System on Site: %s", wferr), nil)
				}

				wid := we.GetID()
				logger.Info().Str("Workflow ID", wid).Msg("executed synchronous create Operating System workflow")

				// Block until the workflow has completed and returned success/error.
				wferr = we.Get(wfCtx, nil)
				if wferr != nil {
					var timeoutErr *tp.TimeoutError
					if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || wfCtx.Err() != nil {
						logger.Error().Err(wferr).Msg("failed to create Operating System, timeout occurred executing workflow on Site.")
						timeoutResp = func() error {
							return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "OperatingSystem", "Create")
						}
						return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Operating System, timeout occurred executing workflow on Site", nil)
					}

					code, uwerr := common.UnwrapWorkflowError(wferr)
					logger.Error().Err(uwerr).Msg("failed to synchronously execute Temporal workflow to create Operating System")
					return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to create Operating System on Site: %s", uwerr), nil)
				}
				logger.Info().Str("Workflow ID", wid).Str("Site ID", ossa.SiteID.String()).Msg("completed synchronous create Operating System workflow")
				return nil
			}()
			if iterErr != nil {
				return iterErr
			}
		}

		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to create Operating System due to DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// Push iPXE / Templated iPXE Operating Systems to associated sites through the
	// generic Core gRPC proxy (Image OSes are synced in-transaction above). Per-site
	// failures are recorded on the association status and do not fail the request.
	if cdbm.IsIPXEType(os.Type) && len(dbossa) > 0 {
		req := apiRequest.ToProto(os)
		siteErrors := syncOperatingSystemToSitesViaProxy(ctx, logger, csh.dbSession, csh.scp, dbossa, createOperatingSystemMethod, req)
		if aerr := updateOperatingSystemAggregateStatus(ctx, logger, csh.dbSession, os.ID, siteErrors > 0, aggregateSyncMessage(siteErrors > 0)); aerr != nil {
			logger.Error().Err(aerr).Msg("failed to update aggregate Operating System status after create sync")
		}
		os, dbossd, dbossa = reloadOperatingSystemForResponse(ctx, logger, csh.dbSession, os, dbossd, dbossa)
	}

	// create response
	apiOperatingSystem := model.NewAPIOperatingSystem(os, dbossd, dbossa, sttsmap)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusCreated, apiOperatingSystem)
}

// reloadOperatingSystemForResponse re-reads the Operating System, its recent status
// details, and its site associations after a proxy sync so the API response reflects
// the post-sync state. Best-effort: the caller passes the values it already holds
// (priorSSDs, priorOSSAs), and each is replaced only on a successful re-read, so a
// read error keeps the prior value rather than dropping it to nil.
func reloadOperatingSystemForResponse(ctx context.Context, logger zerolog.Logger, dbSession *cdb.Session, os *cdbm.OperatingSystem, priorSSDs []cdbm.StatusDetail, priorOSSAs []cdbm.OperatingSystemSiteAssociation) (*cdbm.OperatingSystem, []cdbm.StatusDetail, []cdbm.OperatingSystemSiteAssociation) {
	osDAO := cdbm.NewOperatingSystemDAO(dbSession)
	ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(dbSession)
	sdDAO := cdbm.NewStatusDetailDAO(dbSession)

	reloadedOS := os
	if v, err := osDAO.GetByID(ctx, nil, os.ID, nil); err == nil {
		reloadedOS = v
	} else {
		logger.Warn().Err(err).Msg("failed to reload Operating System for response")
	}

	ssds := priorSSDs
	if v, err := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{os.ID.String()}, common.RECENT_STATUS_DETAIL_COUNT); err == nil {
		ssds = v
	} else {
		logger.Warn().Err(err).Msg("failed to reload Operating System status details for response")
	}

	ossas := priorOSSAs
	if v, _, err := ossaDAO.GetAll(ctx, nil,
		cdbm.OperatingSystemSiteAssociationFilterInput{OperatingSystemIDs: []uuid.UUID{os.ID}},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		[]string{cdbm.SiteRelationName}); err == nil {
		ossas = v
	} else {
		logger.Warn().Err(err).Msg("failed to reload Operating System site associations for response")
	}

	return reloadedOS, ssds, ossas
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllOperatingSystemHandler is the API Handler for getting all OperatingSystems
type GetAllOperatingSystemHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetAllOperatingSystemHandler initializes and returns a new handler for getting all OperatingSystems
func NewGetAllOperatingSystemHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllOperatingSystemHandler {
	return GetAllOperatingSystemHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get all OperatingSystems
// @Description Get all OperatingSystems
// @Tags OperatingSystem
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of Site"
// @Param type query string true "type of Operating System" e.g. 'iPXE', 'Image'"
// @Param status query string false "Filter by status" e.g. 'Pending', 'Error'"
// @Param query query string false "Query input for full text search"
// @Param includeRelation query string false "Related entities to include in response e.g. 'InfrastructureProvider', 'Tenant'"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APIOperatingSystem
// @Router /v2/org/{org}/nico/operating-system [get]
func (gash GetAllOperatingSystemHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("OperatingSystem", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ip, tenant, apiError := common.IsProviderOrTenant(ctx, logger, gash.dbSession, org, dbUser, false, nil)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate pagination request
	pageRequest := pagination.PageRequest{}
	err := c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate request attributes
	err = pageRequest.Validate(cdbm.OperatingSystemOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Visibility rules:
	//   Provider admin: sees only provider-created entries (no tenant entries).
	//   Tenant admin:   sees own entries + provider entries at tenant-accessible sites.
	//   Dual-role:      visibility is the union of both (own tenant + own provider).
	filter := cdbm.OperatingSystemFilterInput{}
	var tenantVisibleProviderSiteIDs []uuid.UUID

	switch {
	case ip != nil && tenant == nil:
		// Provider admin only: sees only provider-created entries.
		filter.InfrastructureProviderID = &ip.ID
	case tenant != nil && ip == nil:
		// Tenant admin only: own entries + provider entries at tenant-accessible sites.
		tenantSiteIDs, tsErr := getTenantSiteIDs(ctx, gash.dbSession, tenant.ID)
		if tsErr != nil {
			logger.Error().Err(tsErr).Msg("error retrieving tenant site IDs for visibility filter")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to determine site access for tenant", nil)
		}
		tenantVisibleProviderSiteIDs = tenantSiteIDs
	case tenant != nil && ip != nil:
		// Dual-role: own tenant + own provider entries, no site restriction.
		filter.TenantIDs = []uuid.UUID{tenant.ID}
		filter.InfrastructureProviderID = &ip.ID
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.OperatingSystemRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// now check siteID in query
	tsDAO := cdbm.NewTenantSiteDAO(gash.dbSession)
	var tenantSites []cdbm.TenantSite
	tenantSitesLoaded := false

	qSiteID := qParams["siteId"]
	if len(qSiteID) > 0 {
		requestedSiteIDs := make([]uuid.UUID, 0, len(qSiteID))
		for _, siteID := range qSiteID {
			parsedSiteID, perr := uuid.Parse(siteID)
			if perr != nil {
				logger.Warn().Err(perr).Str("siteId", siteID).Msg("error parsing Site ID from query string")
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Site specified in query", nil)
			}
			requestedSiteIDs = append(requestedSiteIDs, parsedSiteID)
		}

		siteDAO := cdbm.NewSiteDAO(gash.dbSession)
		sites, _, serr := siteDAO.GetAll(
			ctx,
			nil,
			cdbm.SiteFilterInput{SiteIDs: requestedSiteIDs},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		if serr != nil {
			logger.Error().Err(serr).Msg("error retrieving Sites specified in query")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Sites specified in query, DB error", nil)
		}
		sitesByID := make(map[uuid.UUID]*cdbm.Site, len(sites))
		for i := range sites {
			site := &sites[i]
			sitesByID[site.ID] = site
		}

		tenantSiteIDs := make(map[uuid.UUID]struct{})
		if tenant != nil {
			var terr error
			tenantSites, _, terr = tsDAO.GetAll(
				ctx,
				nil,
				cdbm.TenantSiteFilterInput{
					TenantIDs: []uuid.UUID{tenant.ID},
					SiteIDs:   requestedSiteIDs,
				},
				cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
				nil,
			)
			if terr != nil {
				logger.Error().Err(terr).Msg("error retrieving Tenant Site associations specified in query")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to determine if Tenant has access to Sites specified in query, DB error", nil)
			}
			tenantSitesLoaded = true
			for _, ts := range tenantSites {
				tenantSiteIDs[ts.SiteID] = struct{}{}
			}
		}

		for _, siteID := range requestedSiteIDs {
			site, ok := sitesByID[siteID]
			if !ok {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Site specified in query", nil)
			}
			_, tenantHasAccess := tenantSiteIDs[siteID]
			providerHasAccess := ip != nil && site.InfrastructureProviderID == ip.ID
			if !tenantHasAccess && !providerHasAccess {
				return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Caller is not associated with Site specified in query", nil)
			}
			filter.SiteIDs = append(filter.SiteIDs, siteID)
		}
	}

	// Get query type from query param
	if typeQuery := qParams["type"]; len(typeQuery) > 0 {
		cotel.SetAttribute(handlerSpan, attribute.StringSlice("type", typeQuery))
		for _, typeVal := range typeQuery {
			_, ok := cdbm.OperatingSystemsTypeMap[typeVal]
			if !ok {
				logger.Warn().Msg(fmt.Sprintf("Invalid type value in query: %v", typeVal))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid type value in query", nil)
			}
			filter.OsTypes = append(filter.OsTypes, typeVal)
		}
	}

	// Get query text for full text search from query param
	searchQuery := common.GetSearchQuery(c)
	if searchQuery != nil {
		filter.SearchQuery = searchQuery
		cotel.SetAttribute(handlerSpan, attribute.String("query", *searchQuery))
	}

	// Get status from query param
	if statusQuery := qParams["status"]; len(statusQuery) > 0 {
		cotel.SetAttribute(handlerSpan, attribute.StringSlice("status", statusQuery))
		for _, status := range statusQuery {
			_, ok := cdbm.OperatingSystemStatusMap[status]
			if !ok {
				logger.Warn().Msg(fmt.Sprintf("invalid value in status query: %v", status))
				statusError := validation.Errors{
					"status": errors.New(status),
				}
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Status value in query", statusError)
			}
			filter.Statuses = append(filter.Statuses, status)
		}
	}

	// Get all Operating System by Tenant
	osDAO := cdbm.NewOperatingSystemDAO(gash.dbSession)
	ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(gash.dbSession)

	if tenant != nil && ip == nil {
		mergedOSIDs := make(map[uuid.UUID]struct{})
		selectionPage := cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}

		tenantFilter := filter
		tenantFilter.TenantIDs = []uuid.UUID{tenant.ID}
		tenantOperatingSystems, _, terr := osDAO.GetAll(ctx, nil, tenantFilter, selectionPage, nil)
		if terr != nil {
			logger.Error().Err(terr).Msg("error retrieving tenant-owned Operating Systems from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve tenant-owned Operating Systems", nil)
		}
		for _, os := range tenantOperatingSystems {
			mergedOSIDs[os.ID] = struct{}{}
		}

		providerSiteIDs := tenantVisibleProviderSiteIDs
		if filter.SiteIDs != nil {
			providerSiteIDs = filter.SiteIDs
		}
		if len(providerSiteIDs) > 0 {
			providerAssociations, _, perr := ossaDAO.GetAll(
				ctx,
				nil,
				cdbm.OperatingSystemSiteAssociationFilterInput{SiteIDs: providerSiteIDs},
				selectionPage,
				[]string{cdbm.OperatingSystemRelationName},
			)
			if perr != nil {
				logger.Error().Err(perr).Msg("error retrieving Operating System Site associations visible to Tenant from DB")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve provider-owned Operating Systems", nil)
			}
			for _, association := range providerAssociations {
				if association.OperatingSystem != nil && association.OperatingSystem.InfrastructureProviderID != nil {
					mergedOSIDs[association.OperatingSystemID] = struct{}{}
				}
			}
		}

		mergedIDs := make([]uuid.UUID, 0, len(mergedOSIDs))
		for id := range mergedOSIDs {
			mergedIDs = append(mergedIDs, id)
		}
		filter.TenantIDs = nil
		filter.InfrastructureProviderID = nil
		filter.OperatingSystemIds = mergedIDs
	}

	// Create response
	oss, total, err := osDAO.GetAll(
		ctx,
		nil,
		filter,
		cdbp.PageInput{
			Offset:  pageRequest.Offset,
			Limit:   pageRequest.Limit,
			OrderBy: pageRequest.OrderBy,
		},
		qIncludeRelations,
	)
	if err != nil {
		logger.Error().Err(err).Msg("error getting os from db")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve OperatingSystems", nil)
	}

	// Get status details
	sdDAO := cdbm.NewStatusDetailDAO(gash.dbSession)

	osIDs := []uuid.UUID{}
	sdEntityIDs := []string{}
	for _, os := range oss {
		sdEntityIDs = append(sdEntityIDs, os.ID.String())
		osIDs = append(osIDs, os.ID)
	}

	ssds, serr := sdDAO.GetRecentByEntityIDs(ctx, nil, sdEntityIDs, common.RECENT_STATUS_DETAIL_COUNT)
	if serr != nil {
		logger.Warn().Err(serr).Msg("error retrieving Status Details for Operating Systems from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate status history for Operating Systems", nil)
	}
	ssdMap := map[string][]cdbm.StatusDetail{}
	for _, ssd := range ssds {
		cssd := ssd
		ssdMap[ssd.EntityID] = append(ssdMap[ssd.EntityID], cssd)
	}

	// Get all OperatingSystemSiteAssociations
	var siteIDs []uuid.UUID
	if filter.SiteIDs != nil {
		siteIDs = filter.SiteIDs
	}
	dbossas, _, err := ossaDAO.GetAll(
		ctx,
		nil,
		cdbm.OperatingSystemSiteAssociationFilterInput{
			OperatingSystemIDs: osIDs,
			SiteIDs:            siteIDs,
		},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		[]string{cdbm.SiteRelationName},
	)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Operating System Site associations from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Operating System Site associations from DB", nil)
	}

	// Prepare OperatingSystemSiteAssociation for each OS if it exists
	dbossaMap := map[uuid.UUID][]cdbm.OperatingSystemSiteAssociation{}
	for _, dbossa := range dbossas {
		curVal := dbossa
		dbossaMap[dbossa.OperatingSystemID] = append(dbossaMap[dbossa.OperatingSystemID], curVal)
	}

	// Get all TenantSite records for the Tenant (only relevant when the caller
	// is acting as a Tenant; provider-only admins have no tenant-site context).
	sttsmap := map[uuid.UUID]*cdbm.TenantSite{}

	if tenant != nil {
		if !tenantSitesLoaded {
			var tserr error
			tenantSites, _, tserr = tsDAO.GetAll(
				ctx,
				nil,
				cdbm.TenantSiteFilterInput{
					TenantIDs: []uuid.UUID{tenant.ID},
					SiteIDs:   siteIDs,
				},
				cdbp.PageInput{
					Limit: cutil.GetPtr(cdbp.TotalLimit),
				},
				nil,
			)
			if tserr != nil {
				logger.Error().Err(tserr).Msg("db error retrieving TenantSite records for Tenant")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site associations for Tenant, DB error", nil)
			}
		}

		for _, ts := range tenantSites {
			curVal := ts
			sttsmap[ts.SiteID] = &curVal
		}
	}

	// Create response
	apiOperatingSystems := []*model.APIOperatingSystem{}

	for _, os := range oss {
		curVal := os
		apiOperatingSystem := model.NewAPIOperatingSystem(&curVal, ssdMap[os.ID.String()], dbossaMap[os.ID], sttsmap)
		apiOperatingSystems = append(apiOperatingSystems, apiOperatingSystem)
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

	return c.JSON(http.StatusOK, apiOperatingSystems)

}

// ~~~~~ Get Handler ~~~~~ //

// GetOperatingSystemHandler is the API Handler for retrieving OperatingSystem
type GetOperatingSystemHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetOperatingSystemHandler initializes and returns a new handler to retrieve OperatingSystem
func NewGetOperatingSystemHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetOperatingSystemHandler {
	return GetOperatingSystemHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Retrieve the OperatingSystem
// @Description Retrieve the OperatingSystem
// @Tags OperatingSystem
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of OperatingSystem"
// @Param includeRelation query string false "Related entities to include in response e.g. 'InfrastructureProvider', 'Tenant', 'Site'"
// @Success 200 {object} model.APIOperatingSystem
// @Router /v2/org/{org}/nico/operating-system/{id} [get]
func (gsh GetOperatingSystemHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("OperatingSystem", "Get", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ip, tenant, apiError := common.IsProviderOrTenant(ctx, logger, gsh.dbSession, org, dbUser, false, nil)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.OperatingSystemRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Get os ID from URL param
	osStrID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("operatingsystem_id", osStrID))

	sID, err := uuid.Parse(osStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid OperatingSystem ID in URL", nil)
	}

	osDAO := cdbm.NewOperatingSystemDAO(gsh.dbSession)

	// Check that operating system exists
	os, err := osDAO.GetByID(ctx, nil, sID, qIncludeRelations)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving OperatingSystem DB entity")
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not retrieve OperatingSystem to update", nil)
		}
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Could not retrieve OperatingSystem to update", nil)
	}

	// Visibility check with role-based rules:
	//   Provider admin: can only see provider-owned entries.
	//   Tenant admin:   can see own entries + provider entries at accessible sites.
	//   Dual-role:      can see both tenant and provider entries.
	ownedByTenant := tenant != nil && os.TenantID != nil && *os.TenantID == tenant.ID
	ownedByProvider := ip != nil && os.InfrastructureProviderID != nil && *os.InfrastructureProviderID == ip.ID
	providerVisibilityNeedsSiteCheck := tenant != nil && ip == nil && os.InfrastructureProviderID != nil

	if !ownedByTenant && !ownedByProvider && !providerVisibilityNeedsSiteCheck {
		logger.Warn().Msg("operating system does not belong to the tenant or provider in org")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Operating System does not belong to the tenant or infrastructure provider in org", nil)
	}

	// If caller has dual role (Tenant+Provider) we already know we can go forward.
	// Otherwise we need additional checks:
	if !(tenant != nil && ip != nil) {
		if ip != nil && !ownedByProvider {
			logger.Warn().Msg("provider admin cannot view tenant-owned operating system")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Operating System does not belong to the infrastructure provider in org", nil)
		}
		if providerVisibilityNeedsSiteCheck {
			// Tenant admin seeing a provider-owned entry: verify site-scoped visibility.
			ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(gsh.dbSession)
			ossas, _, ossaErr := ossaDAO.GetAll(ctx, nil,
				cdbm.OperatingSystemSiteAssociationFilterInput{OperatingSystemIDs: []uuid.UUID{os.ID}},
				cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
				nil,
			)
			if ossaErr != nil {
				logger.Error().Err(ossaErr).Msg("error retrieving OS site associations for visibility check")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to verify site access for Operating System", nil)
			}

			tenantSiteIDs, tsErr := getTenantSiteIDs(ctx, gsh.dbSession, tenant.ID)
			if tsErr != nil {
				logger.Error().Err(tsErr).Msg("error retrieving tenant site IDs for visibility check")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to determine site access for tenant", nil)
			}
			tsSet := make(map[uuid.UUID]struct{}, len(tenantSiteIDs))
			for _, sid := range tenantSiteIDs {
				tsSet[sid] = struct{}{}
			}
			visible := false
			for _, ossa := range ossas {
				if _, ok := tsSet[ossa.SiteID]; ok {
					visible = true
					break
				}
			}
			if !visible {
				logger.Warn().Msg("provider-owned OS has no site associations at sites accessible to the tenant")
				return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Operating System is not associated with any site accessible to the caller", nil)
			}
		}
	}

	// get status details for the response
	sdDAO := cdbm.NewStatusDetailDAO(gsh.dbSession)
	ssds, err := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{os.ID.String()}, common.RECENT_STATUS_DETAIL_COUNT)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Status Details for operating system from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Status Details for OperatingSystem", nil)
	}

	dbossas := []cdbm.OperatingSystemSiteAssociation{}
	sttsmap := map[uuid.UUID]*cdbm.TenantSite{}
	if os.Type == cdbm.OperatingSystemTypeImage {
		// Get all OperatingSystemSiteAssociations
		ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(gsh.dbSession)
		dbossas, _, err = ossaDAO.GetAll(
			ctx,
			nil,
			cdbm.OperatingSystemSiteAssociationFilterInput{
				OperatingSystemIDs: []uuid.UUID{os.ID},
			},
			cdbp.PageInput{
				Limit: cutil.GetPtr(cdbp.TotalLimit),
			},
			[]string{cdbm.SiteRelationName},
		)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Operating System Site associations from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Operating System Site associations from DB", nil)
		}

		// Get all TenantSite records for the Tenant
		tsDAO := cdbm.NewTenantSiteDAO(gsh.dbSession)
		tss, _, err := tsDAO.GetAll(
			ctx,
			nil,
			cdbm.TenantSiteFilterInput{
				TenantIDs: []uuid.UUID{tenant.ID},
			},
			cdbp.PageInput{
				Limit: cutil.GetPtr(cdbp.TotalLimit),
			},
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
	}

	// Send response
	apiInstance := model.NewAPIOperatingSystem(os, ssds, dbossas, sttsmap)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiInstance)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateOperatingSystemHandler is the API Handler for updating a OperatingSystem
type UpdateOperatingSystemHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewUpdateOperatingSystemHandler initializes and returns a new handler for updating OperatingSystem
func NewUpdateOperatingSystemHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) UpdateOperatingSystemHandler {
	return UpdateOperatingSystemHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Update an existing OperatingSystem
// @Description Update an existing OperatingSystem
// @Tags OperatingSystem
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of OperatingSystem"
// @Param message body model.APIOperatingSystemUpdateRequest true "OperatingSystem update request"
// @Success 200 {object} model.APIOperatingSystem
// @Router /v2/org/{org}/nico/operating-system/{id} [patch]
func (ush UpdateOperatingSystemHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("OperatingSystem", "Update", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ip, tenant, apiError := common.IsProviderOrTenant(ctx, logger, ush.dbSession, org, dbUser, false, nil)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Get os ID from URL param
	osStrID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("operatingsystem_id", osStrID))

	osID, err := uuid.Parse(osStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid OperatingSystem ID in URL", nil)
	}

	osDAO := cdbm.NewOperatingSystemDAO(ush.dbSession)

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIOperatingSystemUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Check that os exists
	os, err := osDAO.GetByID(ctx, nil, osID, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving OperatingSystem DB entity")
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Operating System with ID specified in URL", nil)
		}
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Could not retrieve OperatingSystem to update", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate(os)
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Operating System update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Operating System update request data", verr)
	}

	// Validate and Set UserData
	verr = apiRequest.ValidateAndSetUserData(ush.cfg.GetSitePhoneHomeUrl(), os)
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating user data in Operating System creation request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating user data in Operating System creation request", verr)
	}

	// Enforce ownership: both roles are evaluated independently so a dual-role
	// caller is permitted if either role authorizes the operation.
	ownedByTenant := tenant != nil && os.TenantID != nil && *os.TenantID == tenant.ID && os.InfrastructureProviderID == nil
	ownedByProvider := false
	if ip != nil && os.InfrastructureProviderID != nil {
		if *os.InfrastructureProviderID != ip.ID {
			logger.Warn().Msg("provider admin cannot update operating system owned by a different provider")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Provider Admin can only update Operating Systems owned by their own provider", nil)
		}
		ownedByProvider = true
	}
	if !ownedByProvider && !ownedByTenant {
		if ip != nil && tenant == nil {
			logger.Warn().Msg("provider admin cannot update tenant-owned operating system")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Provider Admin can only update provider-owned Operating Systems", nil)
		}
		if tenant != nil && ip == nil {
			logger.Warn().Msg("tenant admin cannot update provider-owned operating system")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant Admin can only update their own Operating Systems", nil)
		}
		logger.Warn().Msg("user does not have permission to update this operating system")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Operating System does not belong to your tenant or infrastructure provider", nil)
	}

	// Check for name uniqueness within the owner's scope (provider or tenant).
	if apiRequest.Name != nil && *apiRequest.Name != os.Name {
		uniquenessFilter := cdbm.OperatingSystemFilterInput{Names: []string{*apiRequest.Name}}
		if os.InfrastructureProviderID != nil {
			uniquenessFilter.InfrastructureProviderID = os.InfrastructureProviderID
		} else {
			uniquenessFilter.TenantIDs = []uuid.UUID{tenant.ID}
		}
		oss, tot, serr := osDAO.GetAll(ctx, nil, uniquenessFilter, cdbp.PageInput{}, nil)
		if serr != nil {
			logger.Error().Err(serr).Msg("db error checking for name uniqueness of os")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to update OperatingSystem due to DB error", nil)
		}
		if tot > 0 {
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Another Operating System with specified name already exists", validation.Errors{
				"id": errors.New(oss[0].ID.String()),
			})
		}
	}

	dbossas := []cdbm.OperatingSystemSiteAssociation{}
	sttsmap := map[uuid.UUID]*cdbm.TenantSite{}
	ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(ush.dbSession)
	tsDAO := cdbm.NewTenantSiteDAO(ush.dbSession)

	// Verify Tenant Site Association
	// Verify if Site is in Registered state
	if os.Type == cdbm.OperatingSystemTypeImage {
		dbossas, _, err = ossaDAO.GetAll(
			ctx,
			nil,
			cdbm.OperatingSystemSiteAssociationFilterInput{
				OperatingSystemIDs: []uuid.UUID{os.ID},
			},
			cdbp.PageInput{},
			[]string{cdbm.SiteRelationName},
		)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Operating System Site associations from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Operating System Site associations from DB", nil)
		}

		// Get all TenantSite records for the Tenant
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

		// Verify if associated Site is not registered state
		// Verify if current tenant not associated Site
		for _, dbosa := range dbossas {
			if dbosa.Site.Status != cdbm.SiteStatusRegistered {
				logger.Warn().Msg(fmt.Sprintf("unable to update Operating System. Site: %s. Site is not in Registered state", dbosa.Site.Name))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to update Operating System, Associated Site: %s is not in Registered state", dbosa.Site.Name), nil)
			}

			// Validate the TenantSite exists for current tenant and this site
			_, ok := sttsmap[dbosa.SiteID]
			if !ok {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Unable to update associate Operating System with Site: %s, Tenant does not have access to Site", dbosa.Site.Name), nil)
			}
		}
	}

	// For a Templated iPXE Operating System, verify the effective iPXE template (the
	// request's template when changing it, otherwise the current one) is available at
	// its associated Site before updating and re-pushing it. This mirrors the
	// create-time check and also catches a request switching to a template that is
	// not present at the OS's Site.
	if os.Type == cdbm.OperatingSystemTypeTemplatedIPXE {
		templatedOssas, _, oerr := ossaDAO.GetAll(ctx, nil,
			cdbm.OperatingSystemSiteAssociationFilterInput{OperatingSystemIDs: []uuid.UUID{os.ID}},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if oerr != nil {
			logger.Error().Err(oerr).Msg("error retrieving Operating System Site associations for iPXE template validation")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Operating System Site associations from DB", nil)
		}
		if len(templatedOssas) != 1 {
			logger.Warn().Int("siteAssociationCount", len(templatedOssas)).Msg("unable to update Templated iPXE Operating System unless it is associated with exactly one Site")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Templated iPXE Operating System must be associated with exactly one Site to be updated", nil)
		}
		effectiveTemplateID := os.IpxeTemplateId
		if apiRequest.IpxeTemplateId != nil {
			effectiveTemplateID = apiRequest.IpxeTemplateId
		}
		if effectiveTemplateID != nil {
			targetSiteIDs := make([]uuid.UUID, 0, len(templatedOssas))
			for _, o := range templatedOssas {
				targetSiteIDs = append(targetSiteIDs, o.SiteID)
			}
			if apiErr := validateIpxeTemplateAvailableAtSites(ctx, ush.dbSession, logger, *effectiveTemplateID, targetSiteIDs); apiErr != nil {
				return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
			}
		}
	}

	// Save update status in DB
	osStatus := cutil.GetPtr(cdbm.OperatingSystemStatusReady)
	osStatusMessage := "Operating System has been updated and ready for use"
	if apiRequest.IsActive != nil && !*apiRequest.IsActive {
		osStatus = cutil.GetPtr(cdbm.OperatingSystemStatusDeactivated)
		osStatusMessage = "Operating System has been deactivated"
		if apiRequest.DeactivationNote != nil && *apiRequest.DeactivationNote != "" {
			osStatusMessage += ". " + *apiRequest.DeactivationNote
		}
	} else {
		if apiRequest.IsActive != nil && *apiRequest.IsActive {
			osStatusMessage = "Operating System has been reactivated and is ready for use"
		}
		if os.Type == cdbm.OperatingSystemTypeImage || os.Type == cdbm.OperatingSystemTypeTemplatedIPXE {
			osStatus = cutil.GetPtr(cdbm.OperatingSystemStatusSyncing)
			osStatusMessage = "received Operating System update request, syncing"
		}
	}

	// Values needed after the transaction closure
	var uos *cdbm.OperatingSystem
	var ssds []cdbm.StatusDetail
	// timeoutResp captures any post-rollback work (terminating timed-out
	// Temporal workflows) that must run after the transaction has been rolled
	// back. It is invoked after the closure if non-nil.
	var timeoutResp func() error

	err = cdb.WithTx(ctx, ush.dbSession, func(tx *cdb.Tx) error {
		// When switching from inactive to active, clear deactivation note
		deactivationNote := apiRequest.DeactivationNote
		if apiRequest.IsActive != nil && *apiRequest.IsActive {
			deactivationNote = nil
			_, derr := osDAO.Clear(ctx, tx, cdbm.OperatingSystemClearInput{OperatingSystemId: osID, DeactivationNote: true})
			if derr != nil {
				logger.Error().Err(derr).Msg("error updating/clearing Operating System in DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update/clear Operating System", nil)
			}
		}
		updatedOs, derr := osDAO.Update(ctx, tx, cdbm.OperatingSystemUpdateInput{
			OperatingSystemId:      osID,
			Name:                   apiRequest.Name,
			Description:            apiRequest.Description,
			ImageURL:               apiRequest.ImageURL,
			ImageSHA:               apiRequest.ImageSHA,
			ImageAuthType:          apiRequest.ImageAuthType,
			ImageAuthToken:         apiRequest.ImageAuthToken,
			ImageDisk:              apiRequest.ImageDisk,
			RootFsId:               apiRequest.RootFsID,
			RootFsLabel:            apiRequest.RootFsLabel,
			IpxeScript:             apiRequest.IpxeScript,
			IpxeTemplateId:         apiRequest.IpxeTemplateId,
			IpxeTemplateParameters: apiRequest.IpxeTemplateParameters.ToDBModelPtr(),
			IpxeTemplateArtifacts:  apiRequest.IpxeTemplateArtifacts.ToDBModelPtr(),
			UserData:               apiRequest.UserData,
			AllowOverride:          apiRequest.AllowOverride,
			PhoneHomeEnabled:       apiRequest.PhoneHomeEnabled,
			IsActive:               apiRequest.IsActive,
			DeactivationNote:       deactivationNote,
			Status:                 osStatus,
		})
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating Operating System in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Operating System", nil)
		}
		uos = updatedOs
		logger.Info().Msg("done updating os in DB")

		sdDAO := cdbm.NewStatusDetailDAO(ush.dbSession)
		_, derr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: uos.ID.String(), Status: *osStatus, Message: &osStatusMessage})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create status detail for Operating System update", nil)
		}

		// get status details for the response
		retssds, _, derr := sdDAO.GetAll(ctx, tx, cdbm.StatusDetailFilterInput{EntityIDs: []string{uos.ID.String()}}, cdbp.PageInput{Limit: cutil.GetPtr(pagination.MaxPageSize)})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Status Details for os from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Status Details for Operating System", nil)
		}
		ssds = retssds

		// If OS is Image based, update version too
		// Retrieve Operating System Associations details
		// Trigger workflows to sync Image based Operating System with various Sites
		if uos.Type == cdbm.OperatingSystemTypeImage {
			for _, dbossa := range dbossas {
				_, derr := ossaDAO.Update(
					ctx,
					tx,
					cdbm.OperatingSystemSiteAssociationUpdateInput{
						OperatingSystemSiteAssociationID: dbossa.ID,
						Status:                           cutil.GetPtr(cdbm.OperatingSystemSiteAssociationStatusSyncing),
					},
				)
				if derr != nil {
					logger.Error().Err(derr).Msg("unable to update the Operating System association record in DB")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Operating System Site Association status, DB error", nil)
				}

				// Create Status details
				_, derr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: dbossa.ID.String(), Status: *cutil.GetPtr(cdbm.OperatingSystemSiteAssociationStatusSyncing), Message: cutil.GetPtr("received Operating System Association update request, syncing")})
				if derr != nil {
					logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for Operating System Site Association", nil)
				}

				// Update Operating System Association version
				updatedOssa, derr := ossaDAO.GenerateAndUpdateVersion(ctx, tx, dbossa.ID)
				if derr != nil {
					logger.Error().Err(derr).Msg("error updating version for updated Operating System Association")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to set version for updated Operating System Site Association, DB error", nil)
				}

				// Get the temporal client for the site we are working with.
				stc, derr := ush.scp.GetClientByID(dbossa.SiteID)
				if derr != nil {
					logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
				}

				updateOsRequest := apiRequest.ToImageProto(uos, tenant.Org)

				workflowOptions := temporalClient.StartWorkflowOptions{
					ID:                       "image-os-update-" + updatedOssa.SiteID.String() + "-" + uos.ID.String() + "-" + *updatedOssa.Version,
					WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
					TaskQueue:                queue.SiteTaskQueue,
				}

				logger.Info().Str("Site ID", dbossa.SiteID.String()).Msg("triggering Image based Operating System update workflow ")

				// Workflow execution wrapped in a function literal so `defer cancel()`
				// scopes to this iteration; otherwise the deferred cancels would pile
				// up until the WithTx closure returns.
				iterErr := func() *cutil.APIError {
					// Add context deadlines
					wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
					defer cancel()

					// Trigger Site workflow
					we, wferr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "UpdateOsImage", updateOsRequest)
					if wferr != nil {
						logger.Error().Err(wferr).Msg("failed to synchronously start Temporal workflow to update Operating System")
						return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed start sync workflow to update Operating System on Site: %s", wferr), nil)
					}

					wid := we.GetID()
					logger.Info().Str("Workflow ID", wid).Msg("executed synchronous update Operating System workflow")

					// Block until the workflow has completed and returned success/error.
					wferr = we.Get(wfCtx, nil)
					if wferr != nil {
						var timeoutErr *tp.TimeoutError
						if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || wfCtx.Err() != nil {
							logger.Error().Err(wferr).Msg("failed to update Operating System, timeout occurred executing workflow on Site.")
							timeoutResp = func() error {
								return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "OperatingSystem", "Update")
							}
							return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Operating System, timeout occurred executing workflow on Site", nil)
						}
						code, uwerr := common.UnwrapWorkflowError(wferr)
						logger.Error().Err(uwerr).Msg("failed to synchronously execute Temporal workflow to update Operating System")
						return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to update Operating System on Site: %s", uwerr), nil)
					}
					logger.Info().Str("Workflow ID", wid).Str("Site ID", dbossa.SiteID.String()).Msg("completed synchronous update Operating System workflow")
					return nil
				}()
				if iterErr != nil {
					return iterErr
				}
			}

			// Re-read the site associations so the response reflects the
			// status/version writes we just made (the dbossas slice loaded
			// pre-tx is now stale).
			refreshedOssas, _, derr := ossaDAO.GetAll(
				ctx,
				tx,
				cdbm.OperatingSystemSiteAssociationFilterInput{
					OperatingSystemIDs: []uuid.UUID{uos.ID},
				},
				cdbp.PageInput{
					Limit: cutil.GetPtr(cdbp.TotalLimit),
				},
				[]string{cdbm.SiteRelationName},
			)
			if derr != nil {
				logger.Error().Err(derr).Msg("error refreshing Operating System Site associations from DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve updated Operating System Site associations from DB", nil)
			}
			dbossas = refreshedOssas
		}

		// Templated iPXE updates re-push the definition to its associated Site via
		// the Core proxy after commit. Mark the association (and its status detail)
		// Syncing inside this transaction so the in-flight state is durable before any
		// proxy update runs: validateTemplatedIpxeOsForSite gates Instance selection on
		// a Synced association, so this prevents an Instance from being created against a
		// definition that is mid-update. The post-commit proxy sync transitions the
		// association to Synced or Error.
		if uos.Type == cdbm.OperatingSystemTypeTemplatedIPXE {
			tmplOssas, _, derr := ossaDAO.GetAll(
				ctx,
				tx,
				cdbm.OperatingSystemSiteAssociationFilterInput{OperatingSystemIDs: []uuid.UUID{uos.ID}},
				cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
				nil,
			)
			if derr != nil {
				logger.Error().Err(derr).Msg("error retrieving Operating System Site associations for templated iPXE update")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Operating System Site associations, DB error", nil)
			}
			if len(tmplOssas) != 1 {
				logger.Warn().Int("siteAssociationCount", len(tmplOssas)).Msg("unable to update Templated iPXE Operating System unless it is associated with exactly one Site")
				return cutil.NewAPIError(http.StatusBadRequest, "Templated iPXE Operating System must be associated with exactly one Site to be updated", nil)
			}
			for _, tossa := range tmplOssas {
				if _, derr := ossaDAO.Update(ctx, tx, cdbm.OperatingSystemSiteAssociationUpdateInput{
					OperatingSystemSiteAssociationID: tossa.ID,
					Status:                           cutil.GetPtr(cdbm.OperatingSystemSiteAssociationStatusSyncing),
				}); derr != nil {
					logger.Error().Err(derr).Msg("unable to update the Operating System association record in DB")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Operating System Site Association status, DB error", nil)
				}
				if _, derr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{
					EntityID: tossa.ID.String(),
					Status:   cdbm.OperatingSystemSiteAssociationStatusSyncing,
					Message:  cutil.GetPtr("received Operating System Association update request, syncing"),
				}); derr != nil {
					logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for Operating System Site Association", nil)
				}
			}
		}

		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to update Operating System due to DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// Push iPXE / Templated iPXE Operating System updates to associated sites via the
	// generic Core gRPC proxy (Image OSes are synced in-transaction above). Raw iPXE
	// OSes without site associations have nothing to push.
	if cdbm.IsIPXEType(uos.Type) {
		ipxeOssas, _, oerr := ossaDAO.GetAll(ctx, nil,
			cdbm.OperatingSystemSiteAssociationFilterInput{OperatingSystemIDs: []uuid.UUID{uos.ID}},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			[]string{cdbm.SiteRelationName})
		if oerr != nil {
			logger.Error().Err(oerr).Msg("error retrieving Operating System Site associations for proxy sync")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Operating System Site associations from DB", nil)
		}
		if len(ipxeOssas) > 0 {
			req := apiRequest.ToProto(uos)
			siteErrors := syncOperatingSystemToSitesViaProxy(ctx, logger, ush.dbSession, ush.scp, ipxeOssas, updateOperatingSystemMethod, req)
			if aerr := updateOperatingSystemAggregateStatus(ctx, logger, ush.dbSession, uos.ID, siteErrors > 0, aggregateSyncMessage(siteErrors > 0)); aerr != nil {
				logger.Error().Err(aerr).Msg("failed to update aggregate Operating System status after update sync")
			}
			uos, ssds, dbossas = reloadOperatingSystemForResponse(ctx, logger, ush.dbSession, uos, ssds, dbossas)
		}
	}

	// Send response
	apiOperatingSystem := model.NewAPIOperatingSystem(uos, ssds, dbossas, sttsmap)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiOperatingSystem)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteOperatingSystemHandler is the API Handler for deleting a OperatingSystem
type DeleteOperatingSystemHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteOperatingSystemHandler initializes and returns a new handler for deleting OperatingSystem
func NewDeleteOperatingSystemHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) DeleteOperatingSystemHandler {
	return DeleteOperatingSystemHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Delete an existing OperatingSystem
// @Description Delete an existing OperatingSystem
// @Tags OperatingSystem
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of OperatingSystem"
// @Success 202
// @Router /v2/org/{org}/nico/operating-system/{id} [delete]
func (dsh DeleteOperatingSystemHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("OperatingSystem", "Delete", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ip, tenant, apiError := common.IsProviderOrTenant(ctx, logger, dsh.dbSession, org, dbUser, false, nil)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Get operating system ID from URL param
	osStrID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("operatingsystem_id", osStrID))

	osID, err := uuid.Parse(osStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Operating System ID in URL", nil)
	}

	// Check that operating system exists
	osDAO := cdbm.NewOperatingSystemDAO(dsh.dbSession)
	os, err := osDAO.GetByID(ctx, nil, osID, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Operating System DB entity")
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not retrieve Operating System to delete", nil)
		}
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Could not retrieve Operating System to delete", nil)
	}

	// Enforce ownership: both roles are evaluated independently so a dual-role
	// caller is permitted if either role authorizes the operation.
	ownedByTenant := tenant != nil && os.TenantID != nil && *os.TenantID == tenant.ID && os.InfrastructureProviderID == nil
	ownedByProvider := false
	if ip != nil && os.InfrastructureProviderID != nil {
		if *os.InfrastructureProviderID != ip.ID {
			logger.Warn().Msg("provider admin cannot delete operating system owned by a different provider")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Provider Admin can only delete Operating Systems owned by their own provider", nil)
		}
		ownedByProvider = true
	}
	if !ownedByProvider && !ownedByTenant {
		logger.Warn().Msg("user does not have permission to delete this operating system")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Operating System does not belong to current org's Tenant or Infrastructure Provider", nil)
	}

	// Verify if tenant associated with Site in case of Image based OS
	// Verify Tenant Site Association
	// Verify if Site is in Registered state
	ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(dsh.dbSession)
	ossasToDelete := []cdbm.OperatingSystemSiteAssociation{}
	// Image and Templated iPXE Operating Systems propagate deletes to their
	// associated sites (Image via OsImage workflows, Templated iPXE via the Core
	// gRPC proxy), so their associations must be loaded.
	if os.Type == cdbm.OperatingSystemTypeImage || os.Type == cdbm.OperatingSystemTypeTemplatedIPXE {
		ossasToDelete, _, err = ossaDAO.GetAll(
			ctx,
			nil,
			cdbm.OperatingSystemSiteAssociationFilterInput{
				OperatingSystemIDs: []uuid.UUID{os.ID},
			},
			cdbp.PageInput{},
			[]string{cdbm.SiteRelationName},
		)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Operating System Site associations from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Operating System Site associations from DB", nil)
		}

		// Verify if associated Site is not registered state (image-based only).
		if os.Type == cdbm.OperatingSystemTypeImage {
			for _, dbosa := range ossasToDelete {
				if dbosa.Site.Status != cdbm.SiteStatusRegistered {
					logger.Warn().Msg(fmt.Sprintf("unable to delete Operating System. Site: %s. is not in Registered state", dbosa.SiteID.String()))
					return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to delete Operating System, Associated Site: %s is not in Registered state", dbosa.Site.Name), nil)
				}
			}
		}
	}

	// verify no instances are using the os
	isDAO := cdbm.NewInstanceDAO(dsh.dbSession)

	instanceFilter := cdbm.InstanceFilterInput{OperatingSystemIDs: []uuid.UUID{os.ID}}
	if tenant != nil {
		instanceFilter.TenantIDs = []uuid.UUID{tenant.ID}
	}
	instances, _, err := isDAO.GetAll(ctx, nil, instanceFilter, paginator.PageInput{}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Instances for Operating System from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instances for deleting operatingsystem", nil)
	}

	if len(instances) > 0 {
		logger.Warn().Msg("Instances exist for Operating System, cannot delete it")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Operating System is being used by one or more Instances and cannot be deleted", nil)
	}

	// timeoutResp captures any post-rollback work (terminating timed-out
	// Temporal workflows) that must run after the transaction has been rolled
	// back. It is invoked after the closure if non-nil.
	var timeoutResp func() error

	err = cdb.WithTx(ctx, dsh.dbSession, func(tx *cdb.Tx) error {
		// acquire an advisory lock on the Operating System on which there could be contention
		// this lock is released when the transaction commits or rollsback
		derr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(os.ID.String()), nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("Failed to acquire advisory lock on Operating System")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Operating System, could not acquire data store lock on Operating System", nil)
		}

		// Verify if OS is image based
		if os.Type == cdbm.OperatingSystemTypeImage {

			// Update Operating System to set status to Deleting
			_, derr := osDAO.Update(ctx, tx, cdbm.OperatingSystemUpdateInput{OperatingSystemId: os.ID, Status: cutil.GetPtr(cdbm.OperatingSystemStatusDeleting)})
			if derr != nil {
				logger.Error().Err(derr).Msg("error updating Operating System in DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Operating System", nil)
			}

			// Create status detail
			sdDAO := cdbm.NewStatusDetailDAO(dsh.dbSession)
			// create a status detail record for the Operating System
			_, derr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: os.ID.String(), Status: cdbm.OperatingSystemStatusDeleting, Message: cutil.GetPtr("received request for deletion, pending processing")})
			if derr != nil {
				logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for Operating System", nil)
			}

			// Update Status Deleting for Operating System Association
			for _, ossa := range ossasToDelete {
				if ossa.Status != cdbm.OperatingSystemSiteAssociationStatusDeleting {
					// Update Operating System Association to set status to Deleting
					_, derr := ossaDAO.Update(
						ctx,
						tx,
						cdbm.OperatingSystemSiteAssociationUpdateInput{
							OperatingSystemSiteAssociationID: ossa.ID,
							Status:                           cutil.GetPtr(cdbm.OperatingSystemSiteAssociationStatusDeleting),
						},
					)
					if derr != nil {
						logger.Error().Err(derr).Msg("error updating Operating System Association in DB")
						return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Operating Systems", nil)
					}

					// create a status detail record for the Operating System Association
					_, derr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: ossa.ID.String(), Status: cdbm.OperatingSystemSiteAssociationStatusDeleting, Message: cutil.GetPtr("received request for deletion, pending processing")})
					if derr != nil {
						logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
						return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for Operating System Association", nil)
					}

					// Get the temporal client for the site we are working with.
					stc, derr := dsh.scp.GetClientByID(ossa.SiteID)
					if derr != nil {
						logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
						return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
					}

					// Prepare the delete/release request workflow object
					deleteOsRequest := os.ToImageDeletionRequestProto(tenant.Org)

					workflowOptions := temporalClient.StartWorkflowOptions{
						ID:                       "image-os-delete-" + ossa.SiteID.String() + "-" + os.ID.String() + "-" + *ossa.Version,
						WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
						TaskQueue:                queue.SiteTaskQueue,
					}

					logger.Info().Msg("triggering Operating System delete workflow")

					// Workflow execution wrapped in a function literal so `defer cancel()`
					// scopes to this iteration; otherwise the deferred cancels would pile
					// up until the WithTx closure returns.
					iterErr := func() *cutil.APIError {
						wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
						defer cancel()

						// Trigger Site workflow to delete Image based OperatingSystem
						we, wferr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "DeleteOsImage", deleteOsRequest)
						if wferr != nil {
							logger.Error().Err(wferr).Msg("failed to synchronously start Temporal workflow to delete Operating System")
							return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to start sync workflow to delete Operating System on Site: %s", wferr), nil)
						}

						wid := we.GetID()
						logger.Info().Str("Workflow ID", wid).Msg("executed synchronous delete Operating System workflow")

						// Execute the workflow synchronously
						wferr = we.Get(wfCtx, nil)
						// Handle skippable errors
						if wferr != nil {
							// If this was a 404 back from NICo, we can treat the object as already having been deleted and allow things to proceed.
							var applicationErr *tp.ApplicationError
							if errors.As(wferr, &applicationErr) && slices.Contains(swe.ObjectNotFoundErrTypes(), applicationErr.Type()) {
								logger.Warn().Msg(swe.ErrTypeNICoObjectNotFound + " received from Site")
								// Reset error to nil
								wferr = nil
							}
						}

						// Check if err is still nil now that we've handled any skippable errors.
						if wferr != nil {
							var timeoutErr *tp.TimeoutError
							if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || wfCtx.Err() != nil {
								logger.Error().Err(wferr).Msg("failed to delete Operating System, timeout occurred executing workflow on Site.")
								timeoutResp = func() error {
									return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "OperatingSystem", "Delete")
								}
								return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Operating System, timeout occurred executing workflow on Site", nil)
							}

							code, uwerr := common.UnwrapWorkflowError(wferr)
							logger.Error().Err(uwerr).Msg("failed to synchronously execute Temporal workflow to delete Operating System")
							return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to delete Operating System on Site: %s", uwerr), nil)
						}

						logger.Info().Str("Workflow ID", wid).Msg("completed synchronous delete Operating System workflow")
						return nil
					}()
					if iterErr != nil {
						return iterErr
					}
				}
			}
		}

		// Templated iPXE Operating Systems mark the OS (and associations) as
		// Deleting in-transaction; the deletes are pushed to sites via the Core
		// gRPC proxy after commit, and the OS is soft-deleted once all sites are
		// cleaned up.
		if os.Type == cdbm.OperatingSystemTypeTemplatedIPXE && len(ossasToDelete) > 0 {
			if _, derr := osDAO.Update(ctx, tx, cdbm.OperatingSystemUpdateInput{OperatingSystemId: os.ID, Status: cutil.GetPtr(cdbm.OperatingSystemStatusDeleting)}); derr != nil {
				logger.Error().Err(derr).Msg("error updating Operating System in DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Operating System", nil)
			}
			sdDAO := cdbm.NewStatusDetailDAO(dsh.dbSession)
			if _, derr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{
				EntityID: os.ID.String(),
				Status:   cdbm.OperatingSystemStatusDeleting,
				Message:  cutil.GetPtr("received request for deletion, pending processing"),
			}); derr != nil {
				logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for Operating System", nil)
			}

			// Mark each affected association Deleting in the same transaction so the
			// in-progress state is durable before any per-site proxy delete runs; the
			// post-commit loop transitions each to deleted (row removed) or Error.
			for _, ossa := range ossasToDelete {
				if ossa.Status == cdbm.OperatingSystemSiteAssociationStatusDeleting {
					continue
				}
				if _, derr := ossaDAO.Update(ctx, tx, cdbm.OperatingSystemSiteAssociationUpdateInput{
					OperatingSystemSiteAssociationID: ossa.ID,
					Status:                           cutil.GetPtr(cdbm.OperatingSystemSiteAssociationStatusDeleting),
				}); derr != nil {
					logger.Error().Err(derr).Msg("error updating Operating System Association in DB")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Operating System", nil)
				}
				if _, derr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{
					EntityID: ossa.ID.String(),
					Status:   cdbm.OperatingSystemSiteAssociationStatusDeleting,
					Message:  cutil.GetPtr("received request for deletion, pending processing"),
				}); derr != nil {
					logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for Operating System Association", nil)
				}
			}
		}

		// Delete OS if its not Image
		// Delete OS if there is no Operating Site Association in case of Image based OS
		if os.Type == cdbm.OperatingSystemTypeIPXE || len(ossasToDelete) == 0 {
			derr := osDAO.Delete(ctx, tx, os.ID)
			if derr != nil {
				logger.Error().Err(derr).Msg("error deleting Operating System record in DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Error deleting Operating System record in DB", nil)
			}
		}

		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to delete Operating System due to DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// Push deletes for Templated iPXE Operating Systems to associated sites via the
	// generic Core gRPC proxy, remove the synced associations, and soft-delete the
	// OS once every site is cleaned up. A not-found object on a site is treated as
	// already deleted.
	if os.Type == cdbm.OperatingSystemTypeTemplatedIPXE && len(ossasToDelete) > 0 {
		req := os.ToDeletionRequestProto()
		remaining := 0
		for _, ossa := range ossasToDelete {
			slogger := logger.With().Str("Site ID", ossa.SiteID.String()).Logger()
			stc, cerr := dsh.scp.GetClientByID(ossa.SiteID)
			if cerr != nil {
				slogger.Error().Err(cerr).Msg("failed to retrieve Temporal client for Site")
				_ = updateOSSAStatusViaProxy(ctx, slogger, dsh.dbSession, ossa.ID, cdbm.OperatingSystemSiteAssociationStatusError, "failed to connect to site")
				remaining++
				continue
			}
			perr := common.ExecuteCoreGRPC(ctx, stc, deleteOperatingSystemMethod, req, nil, ossa.SiteID.String())
			if perr != nil {
				if perr.Code == http.StatusNotFound {
					slogger.Warn().Msg("Operating System not found on site, treating delete as successful")
				} else {
					slogger.Error().Err(perr).Int("code", perr.Code).Msg("failed to delete Operating System on site via Core proxy")
					_ = updateOSSAStatusViaProxy(ctx, slogger, dsh.dbSession, ossa.ID, cdbm.OperatingSystemSiteAssociationStatusError, "failed to delete Operating System on site")
					remaining++
					continue
				}
			}
			if derr := ossaDAO.Delete(ctx, nil, ossa.ID); derr != nil {
				slogger.Error().Err(derr).Msg("failed to delete Operating System Site Association after site delete")
				_ = updateOSSAStatusViaProxy(ctx, slogger, dsh.dbSession, ossa.ID, cdbm.OperatingSystemSiteAssociationStatusError, "failed to remove Operating System site association after site delete")
				remaining++
			}
		}
		if remaining == 0 {
			if derr := osDAO.Delete(ctx, nil, os.ID); derr != nil {
				logger.Error().Err(derr).Msg("failed to soft-delete Operating System after all sites cleaned up")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to delete Operating System", nil)
			}
		} else {
			// Some sites could not be cleaned up. Transition the OS out of the
			// transient Deleting state into Error (with per-site Error associations
			// recorded above) so it is not stranded in Deleting forever: the state is
			// visible via GET and the delete is idempotently retryable (already-gone
			// sites return not-found and cleaned-up associations are no longer
			// candidates), converging once the sites recover.
			if aerr := updateOperatingSystemAggregateStatus(ctx, logger, dsh.dbSession, os.ID, true, "failed to delete Operating System from one or more sites"); aerr != nil {
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to update Operating System status", nil)
			}
		}
	}

	// Create response
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusAccepted, model.NewAPIDeletionAcceptedResponse())

}
