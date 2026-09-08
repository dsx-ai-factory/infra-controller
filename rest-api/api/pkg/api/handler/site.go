// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strconv"
	"strings"
	"time"

	mapset "github.com/deckarep/golang-set/v2"
	"github.com/labstack/echo/v4"

	"go.opentelemetry.io/otel/attribute"
	tOperatorv1 "go.temporal.io/api/operatorservice/v1"
	tWorkflowv1 "go.temporal.io/api/workflowservice/v1"
	tClient "go.temporal.io/sdk/client"
	"google.golang.org/protobuf/types/known/durationpb"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	csm "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/sitemgr"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	siteWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/site"
)

const (
	// SiteRegistrationTokenValidityPeriod is the duration after which a Site registration token expires
	SiteRegistrationTokenValidityPeriod = time.Hour * 24 * 2

	// SiteWorkflowRetentionPeriod is the duration after which completed Site workflows are expunged
	SiteWorkflowRetentionPeriod = time.Hour * 24 * 7
)

// ~~~~~ Create Handler ~~~~~ //

// CreateSiteHandler is the API Handler for creating new Tenant
type CreateSiteHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	tnc        tClient.NamespaceClient
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewCreateSiteHandler initializes and returns a new handler for creating Tenant
func NewCreateSiteHandler(dbSession *cdb.Session, tc tClient.Client, tnc tClient.NamespaceClient, cfg *config.Config) CreateSiteHandler {
	return CreateSiteHandler{
		dbSession:  dbSession,
		tc:         tc,
		tnc:        tnc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Create a Site
// @Description Create a Site for the org. Only Infrastructure Providers can create Site
// @Tags site
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APISiteCreateRequest true "Site create request"
// @Success 201 {object} model.APISite
// @Router /v2/org/{org}/nico/site [post]
func (csh CreateSiteHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Site", "Create", c, csh.tracerSpan)
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

	// Validate role, only Provider Admins are allowed to create Site
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Validate request
	// Bind request data to API model
	apiRequest := model.APISiteCreateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Site creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Site creation request data", verr)
	}

	// Get Infrastructure Provider for this org
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, csh.dbSession, org)
	if err != nil {
		if err == common.ErrOrgInstrastructureProviderNotFound {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound,
				fmt.Sprintf("Org '%v' does not have an Infrastructure Provider", org), nil)
		}
		logger.Error().Err(err).Msg("error retrieving Infrastructure Provider for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Infrastructure Provider", nil)
	}

	// Check for name uniqueness for the Site within the scope of the Provider
	stDAO := cdbm.NewSiteDAO(csh.dbSession)
	sts, tot, err := stDAO.GetAll(ctx, nil, cdbm.SiteFilterInput{Name: &apiRequest.Name, InfrastructureProviderIDs: []uuid.UUID{ip.ID}}, paginator.PageInput{}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("db error checking for name uniqueness of Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create Site, error reading from data store", nil)
	}
	if tot > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "A Site with specified name already exists for Infrastructure Provider", validation.Errors{
			"id": errors.New(sts[0].ID.String()),
		})
	}

	// siteManagerURL is captured here so that, if the tx ultimately fails
	// after CreateSite succeeded, we can undo the Site Manager entry below.
	var siteManagerURL string
	siteManagerCreated := false
	sdDAO := cdbm.NewStatusDetailDAO(csh.dbSession)

	var st *cdbm.Site
	var ssd *cdbm.StatusDetail

	err = cdb.WithTx(ctx, csh.dbSession, func(tx *cdb.Tx) error {
		// Create Site
		dbCreateInput := cdbm.SiteCreateInput{
			Name:                     apiRequest.Name,
			Description:              apiRequest.Description,
			Org:                      org,
			InfrastructureProviderID: ip.ID,
			IsInfinityEnabled:        false,
			SerialConsoleHostname:    apiRequest.SerialConsoleHostname,
			IsSerialConsoleEnabled:   false,
			Status:                   cdbm.SiteStatusPending,
			CreatedBy:                dbUser.ID,
			// New sites use the default feature posture.
			Config: cdbm.SiteConfig{
				NativeNetworking:     true,
				NetworkSecurityGroup: true,
				Flow:                 true,
			},
		}
		if apiRequest.Location != nil {
			dbCreateInput.Location = &cdbm.SiteLocation{
				City:    apiRequest.Location.City,
				State:   apiRequest.Location.State,
				Country: apiRequest.Location.Country,
			}
		}
		if apiRequest.Contact != nil {
			dbCreateInput.Contact = &cdbm.SiteContact{
				Email: apiRequest.Contact.Email,
			}
		}

		createdSite, derr := stDAO.Create(ctx, tx, dbCreateInput)
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Site DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Site", nil)
		}
		st = createdSite

		// Create status detail
		createdSSD, derr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: st.ID.String(), Status: *cutil.GetPtr(cdbm.SiteStatusPending), Message: cutil.GetPtr("received site creation request, pending pairing")})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for Site", nil)
		}
		if createdSSD == nil {
			logger.Error().Msg("Status Detail DB entry not returned from Create")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to get new Status Detail for Site", nil)
		}
		ssd = createdSSD

		// Create Site entry in Site Manager
		if csh.cfg.GetSiteManagerEnabled() {
			// create site in site manager
			siteManagerURL = csh.cfg.GetSiteManagerEndpoint()
			provider := st.Org
			if st.InfrastructureProvider != nil {
				provider = st.InfrastructureProvider.Name
			}

			derr = csm.CreateSite(ctx, logger, st.ID.String(), st.Name, provider, st.Org, siteManagerURL)
			if derr != nil {
				logger.Error().Err(derr).Msg("failed to create Site entry in Site Manager")
				return cutil.NewAPIError(http.StatusInternalServerError, "Unable to create Site Manager entry for Site", nil)
			}
			siteManagerCreated = true

			// Retrieve registration token (OTP) from Site Manager
			registrationToken, registrationTokenExpires, serr := csm.GetSiteOTP(ctx, logger, st.ID.String(), siteManagerURL)
			if serr != nil {
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to obtain registration token from Site Manager", nil)
			}

			_, derr = stDAO.Update(ctx, tx, cdbm.SiteUpdateInput{
				SiteID:                      st.ID,
				RegistrationToken:           registrationToken,
				RegistrationTokenExpiration: registrationTokenExpires,
			})
			if derr != nil {
				logger.Error().Err(derr).Msg("error updating Site with registration token")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Site with registration token, error communicating with data store", nil)
			}

			// Refresh Site object
			refreshedSite, serr := stDAO.GetByID(ctx, tx, st.ID, nil, false)
			if serr != nil {
				logger.Error().Err(serr).Msg("error retrieving Site from DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to refresh Site data with registration token, error communicating with data store", nil)
			}
			st = refreshedSite
		}

		// Create Temporal namespace
		workflowRetentionPeriod := durationpb.New(SiteWorkflowRetentionPeriod)

		regRequest := tWorkflowv1.RegisterNamespaceRequest{
			Namespace:                        st.ID.String(),
			Description:                      fmt.Sprintf("Namespace for Site %v", st.ID),
			WorkflowExecutionRetentionPeriod: workflowRetentionPeriod,
		}

		derr = csh.tnc.Register(ctx, &regRequest)
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Temporal namespace for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create workflow namespace for Site", nil)
		}

		return nil
	})
	if err != nil {
		// Undo the Site Manager entry if it was created before the tx aborted;
		// the Temporal namespace registration is intentionally not unwound to
		// match the prior behavior (it was likewise left in place on commit
		// failures by the legacy BeginTx/Commit scaffolding).
		if siteManagerCreated {
			csm.DeleteSite(ctx, logger, st.ID.String(), siteManagerURL)
		}
		return common.HandleTxError(c, logger, err, "Failed to create Site, DB transaction error")
	}

	// Create response
	apiSite := model.NewAPISite(*st, []cdbm.StatusDetail{*ssd}, nil)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusCreated, apiSite)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateSiteHandler is the API Handler for updating a Site
type UpdateSiteHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewUpdateSiteHandler initializes and returns a new handler for updating Site
func NewUpdateSiteHandler(dbSession *cdb.Session, tc tClient.Client, cfg *config.Config) UpdateSiteHandler {
	return UpdateSiteHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Update an existing Site
// @Description Update an existing Site for the org
// @Tags site
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Site"
// @Param message body model.APISiteUpdateRequest true "Site update request"
// @Success 200 {object} model.APISite
// @Router /v2/org/{org}/nico/site/{id} [patch]
func (ush UpdateSiteHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Site", "Update", c, ush.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	provider, tenant, apiErr := common.IsProviderOrTenant(ctx, logger, ush.dbSession, org, dbUser, false, nil)
	if apiErr != nil {
		return c.JSON(apiErr.Code, apiErr)
	}

	// Get application instance ID from URL param
	siteStrID := c.Param("id")

	ush.tracerSpan.SetAttribute(handlerSpan, attribute.String("site_id", siteStrID), logger)

	siteID, err := uuid.Parse(siteStrID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Site ID in URL", nil)
	}

	stDAO := cdbm.NewSiteDAO(ush.dbSession)

	// Validate request
	// Bind request data to API model
	apiRequest := model.APISiteUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate(provider != nil, tenant != nil)
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Site update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Site update request data", verr)
	}

	// Check that Site exists
	es, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		logger.Warn().Err(err).Msg("error retrieving Site DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not retrieve Site to update", nil)
	}

	isAssociated := false

	// Check that Site is associated with Provider
	if provider != nil {
		isAssociated = provider.ID == es.InfrastructureProviderID
	}

	tsDAO := cdbm.NewTenantSiteDAO(ush.dbSession)
	var ts *cdbm.TenantSite

	if !isAssociated && tenant != nil {
		// Check if Tenant is associated with Site
		tss, _, err := tsDAO.GetAll(ctx, nil, cdbm.TenantSiteFilterInput{TenantIDs: []uuid.UUID{tenant.ID}, SiteIDs: []uuid.UUID{siteID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Warn().Err(err).Msg("error retrieving TenantSite records from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to determine Tenant's access to Site, DB error", nil)
		}
		if len(tss) > 0 {
			ts = &tss[0]
			isAssociated = true
		}
	}

	if !isAssociated {
		logger.Warn().Msg("Site is not associated with org's Infrastructure Provider or Tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not allowed to modify Site attributes", nil)
	}

	var registrationToken *string
	var registrationTokenExpires *time.Time
	var status *string
	var statusMessage *string

	if apiRequest.RenewRegistrationToken != nil && *apiRequest.RenewRegistrationToken {
		// Re-issue Site OTP using Site Manager
		url := ush.cfg.GetSiteManagerEndpoint()
		err = csm.RollSite(ctx, logger, es.ID.String(), es.Name, url)
		if err != nil {
			logger.Warn().Err(err).Msg("error rolling site in site manager")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site Manager failure", nil)
		}

		registrationToken, registrationTokenExpires, err = csm.GetSiteOTP(ctx, logger, es.ID.String(), url)
		if err != nil {
			logger.Error().Err(err).Msg("error getting site from site manager")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Site Manager failure", nil)
		}

		// Switch Site status only if it is currently not Registered
		if es.Status != cdbm.SiteStatusRegistered {
			status = cutil.GetPtr(cdbm.SiteStatusPending)
			statusMessage = cutil.GetPtr("Registration token renewed, pending pairing")
		}
	}

	// Check for name uniqueness for the Site
	if apiRequest.Name != nil && *apiRequest.Name != es.Name {
		sts, tot, serr := stDAO.GetAll(ctx, nil, cdbm.SiteFilterInput{Name: apiRequest.Name, InfrastructureProviderIDs: []uuid.UUID{es.InfrastructureProviderID}}, paginator.PageInput{}, nil)
		if serr != nil {
			logger.Error().Err(serr).Msg("db error checking for name uniqueness of Site")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to update Site, error reading from data store", nil)
		}
		if tot > 0 {
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Another Site with specified name already exists for Provider", validation.Errors{
				"id": errors.New(sts[0].ID.String()),
			})
		}
	}

	// Check if SOL params changed
	if es.Status != cdbm.SiteStatusRegistered {
		if apiRequest.IsSerialConsoleEnabled != nil && *apiRequest.IsSerialConsoleEnabled != es.IsSerialConsoleEnabled {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Cannot enable/disable Serial Console for Site that is not in Registered state", nil)
		}

		if apiRequest.SerialConsoleIdleTimeout != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Cannot update Serial Console idle timeout for Site that is not in Registered state", nil)
		}

		if apiRequest.SerialConsoleMaxSessionLength != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Cannot update Serial Console max session length for Site that is not in Registered state", nil)
		}
	}

	// Check if Site capabilities can be modified
	if apiRequest.Capabilities != nil {
		if es.Config == nil {
			es.Config = &cdbm.SiteConfig{}
		}
		if apiRequest.Capabilities.NativeNetworking != nil && *apiRequest.Capabilities.NativeNetworking != es.Config.NativeNetworking && !*apiRequest.Capabilities.NativeNetworking {
			// Native Networking is being disabled
			// Check if there are VPCs that have virtualization type set to FNN
			vpcDAO := cdbm.NewVpcDAO(ush.dbSession)
			_, vpcCount, err := vpcDAO.GetAll(ctx, nil, cdbm.VpcFilterInput{SiteIDs: []uuid.UUID{es.ID}, NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN)}, cdbp.PageInput{}, nil)
			if err != nil {
				logger.Error().Err(err).Msg("error retrieving VPCs for Site from DB")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPCs for Site, unable to determine if Native Networking can be disabled", nil)
			}
			if vpcCount > 0 {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Cannot disable Native Networking while Site has one or more VPCs with virtualization type set to FNN", nil)
			}
		}
	}

	sdDAO := cdbm.NewStatusDetailDAO(ush.dbSession)

	var us *cdbm.Site
	var ssds []cdbm.StatusDetail

	err = cdb.WithTx(ctx, ush.dbSession, func(tx *cdb.Tx) error {
		// Update Site
		if provider != nil {
			siteUpdateInput := cdbm.SiteUpdateInput{
				SiteID:                        siteID,
				Name:                          apiRequest.Name,
				Description:                   apiRequest.Description,
				RegistrationToken:             registrationToken,
				RegistrationTokenExpiration:   registrationTokenExpires,
				SerialConsoleHostname:         apiRequest.SerialConsoleHostname,
				IsSerialConsoleEnabled:        apiRequest.IsSerialConsoleEnabled,
				SerialConsoleIdleTimeout:      apiRequest.SerialConsoleIdleTimeout,
				SerialConsoleMaxSessionLength: apiRequest.SerialConsoleMaxSessionLength,
				Status:                        status,
			}
			if apiRequest.Location != nil {
				siteUpdateInput.Location = &cdbm.SiteLocation{
					City:    apiRequest.Location.City,
					State:   apiRequest.Location.State,
					Country: apiRequest.Location.Country,
				}
			}
			if apiRequest.Contact != nil {
				siteUpdateInput.Contact = &cdbm.SiteContact{
					Email: apiRequest.Contact.Email,
				}
			}

			if apiRequest.Capabilities != nil {
				siteUpdateInput.Config = &cdbm.SiteConfigUpdateInput{
					NativeNetworking:          apiRequest.Capabilities.NativeNetworking,
					NetworkSecurityGroup:      apiRequest.Capabilities.NetworkSecurityGroup,
					NVLinkPartition:           apiRequest.Capabilities.NVLinkPartition,
					Flow:                      apiRequest.Capabilities.Flow,
					ImageBasedOperatingSystem: apiRequest.Capabilities.ImageBasedOperatingSystem,
					DPSPowerManagement:        apiRequest.Capabilities.DPSPowerManagement,
				}
			}

			// Update Site
			updated, derr := stDAO.Update(ctx, tx, siteUpdateInput)
			if derr != nil {
				logger.Error().Err(derr).Msg("error updating Site")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Site, DB error", nil)
			}
			us = updated
		}

		// Add Status Detail record if needed
		if status != nil {
			_, derr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: siteID.String(), Status: *status, Message: statusMessage})
			if derr != nil {
				logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for Site", nil)
			}
		}

		// Get status details (inside the tx for read-your-writes consistency
		// with the StatusDetail created above)
		details, _, derr := sdDAO.GetAll(ctx, tx, cdbm.StatusDetailFilterInput{EntityIDs: []string{siteID.String()}}, cdbp.PageInput{Limit: cutil.GetPtr(pagination.MaxPageSize)})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Status Details for Site from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Status Details for Site", nil)
		}
		ssds = details

		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to update Site, DB transaction error")
	}

	// Create response
	apiSite := model.NewAPISite(*us, ssds, ts)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiSite)
}

// ~~~~~ Get Handler ~~~~~ //

// GetSiteHandler is the API Handler for getting a Site
type GetSiteHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetSiteHandler initializes and returns a new handler for getting Site
func NewGetSiteHandler(dbSession *cdb.Session, tc tClient.Client, cfg *config.Config) GetSiteHandler {
	return GetSiteHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get a Site
// @Description Get a Site for the org
// @Tags site
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Site"
// @Param includeRelation query string false "Related entities to include in response e.g. 'InfrastructureProvider'"
// @Success 200 {object} model.APISite
// @Router /v2/org/{org}/nico/site/{id} [get]
func (gsh GetSiteHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Site", "Get", c, gsh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	provider, tenant, apiErr := common.IsProviderOrTenant(ctx, logger, gsh.dbSession, org, dbUser, true, nil)
	if apiErr != nil {
		return c.JSON(apiErr.Code, apiErr)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.SiteRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Get Site ID from URL param
	stStrID := c.Param("id")

	gsh.tracerSpan.SetAttribute(handlerSpan, attribute.String("site_id", stStrID), logger)

	stID, err := uuid.Parse(stStrID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Site ID in URL", nil)
	}

	// Get Site
	stDAO := cdbm.NewSiteDAO(gsh.dbSession)

	st, err := stDAO.GetByID(ctx, nil, stID, qIncludeRelations, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Site with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site", nil)
	}

	// Check Site association
	isAssociated := false

	var ts *cdbm.TenantSite

	if provider != nil {
		isAssociated = provider.ID == st.InfrastructureProviderID
	}

	if !isAssociated && tenant != nil {
		tsDAO := cdbm.NewTenantSiteDAO(gsh.dbSession)

		tss, tsCount, serr := tsDAO.GetAll(ctx, nil, cdbm.TenantSiteFilterInput{TenantIDs: []uuid.UUID{tenant.ID}, SiteIDs: []uuid.UUID{stID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if serr != nil {
			logger.Error().Err(serr).Msg("error retrieving TenantSite from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant/Site association to determine access to Site, DB error", nil)
		}
		if tsCount > 0 {
			ts = &tss[0]
			isAssociated = true
		}

		if !isAssociated {
			enabled, serr := common.TenantHasTargetedInstanceCreation(ctx, nil, gsh.dbSession, tenant, &common.TenantPrivilegeScope{SiteID: &st.ID})
			if serr != nil {
				logger.Error().Err(serr).Msg("error resolving TargetedInstanceCreation for Tenant/Site")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to resolve Tenant capability for Site, DB error", nil)
			}
			isAssociated = enabled
		}
	}

	if !isAssociated {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site is not associated with org", nil)
	}

	// Get Site status details
	sdDAO := cdbm.NewStatusDetailDAO(gsh.dbSession)

	ssds, err := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{stID.String()}, common.RECENT_STATUS_DETAIL_COUNT)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Status Details for Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Status Details for Site", nil)
	}

	// Create response
	ast := model.NewAPISite(*st, ssds, ts)

	return c.JSON(http.StatusOK, ast)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllSiteHandler is the API Handler for retrieving all Sites
type GetAllSiteHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetAllSiteHandler initializes and returns a new handler for retrieving all Sites
func NewGetAllSiteHandler(dbSession *cdb.Session, tc tClient.Client, cfg *config.Config) GetAllSiteHandler {
	return GetAllSiteHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get all Sites
// @Description Get all Sites for the org
// @Tags site
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param isNativeNetworkingEnabled query boolean false "Filter by native networking enabled flag"
// @Param isNetworkSecurityGroupEnabled query boolean false "Filter by network security group enabled flag"
// @Param isNVLinkPartitionEnabled query boolean false "Filter by NVLink partition enabled flag"
// @Param isFlowEnabled query boolean false "Filter by NICo Flow enabled flag"
// @Param query query string false "Query input for full text search"
// @Param status query string false "Query input for status"
// @Param includeRelation query string false "Related entities to include in response e.g. 'InfrastructureProvider'"
// @Success 200 {array} []model.APISite
// @Router /v2/org/{org}/nico/site [get]
func (gash GetAllSiteHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Site", "GetAll", c, gash.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate pagination request
	pageRequest := pagination.PageRequest{}
	err := c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate request attributes
	err = pageRequest.Validate(cdbm.SiteOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.SiteRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	provider, tenant, apiErr := common.IsProviderOrTenant(ctx, logger, gash.dbSession, org, dbUser, true, nil)
	if apiErr != nil {
		return c.JSON(apiErr.Code, apiErr)
	}

	filter := cdbm.SiteFilterInput{}

	searchQuery := common.GetSearchQuery(c)
	if searchQuery != nil {
		gash.tracerSpan.SetAttribute(handlerSpan, attribute.String("query", *searchQuery), logger)
		filter.SearchQuery = searchQuery
	}

	// Get status from query param
	if statusQuery := qParams["status"]; len(statusQuery) > 0 {
		gash.tracerSpan.SetAttribute(handlerSpan, attribute.StringSlice("status", statusQuery), logger)
		for _, status := range statusQuery {
			_, ok := cdbm.SiteStatusMap[status]
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

	// Check `includeMachineStats` in query (absent is treated as false)
	includeMachineStatsPtr, err := common.ParseOptionalBoolQueryParam(c, "includeMachineStats")
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `includeMachineStats` query param", nil)
	}
	includeMachineStats := includeMachineStatsPtr != nil && *includeMachineStatsPtr

	// Check `includeGpuStats` in query (absent is treated as false)
	includeGpuStatsPtr, err := common.ParseOptionalBoolQueryParam(c, "includeGpuStats")
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `includeGpuStats` query param", nil)
	}
	includeGpuStats := includeGpuStatsPtr != nil && *includeGpuStatsPtr

	// Optional boolean site-config filters. A nil pointer means "not provided"
	// (no filter); an explicit true/false filters on that value.
	configFilter := cdbm.SiteConfigFilterInput{}

	configFilter.NativeNetworking, err = common.ParseOptionalBoolQueryParam(c, "isNativeNetworkingEnabled")
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `isNativeNetworkingEnabled` query param", nil)
	}

	configFilter.NetworkSecurityGroup, err = common.ParseOptionalBoolQueryParam(c, "isNetworkSecurityGroupEnabled")
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `isNetworkSecurityGroupEnabled` query param", nil)
	}

	configFilter.NVLinkPartition, err = common.ParseOptionalBoolQueryParam(c, "isNVLinkPartitionEnabled")
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `isNVLinkPartitionEnabled` query param", nil)
	}

	configFilter.Flow, err = common.ParseOptionalBoolQueryParam(c, "isFlowEnabled")
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `isFlowEnabled` query param", nil)
	}

	if configFilter.NativeNetworking != nil || configFilter.NetworkSecurityGroup != nil ||
		configFilter.NVLinkPartition != nil || configFilter.Flow != nil {
		filter.Config = &configFilter
	}

	// Machine and GPU stats are Provider-only; gate once if either is requested.
	if (includeMachineStats || includeGpuStats) && provider == nil {
		logger.Warn().Msg("`includeMachineStats`/`includeGpuStats` are only permitted with Provider Admin role")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User must have Provider Admin role to request `includeMachineStats` or `includeGpuStats`", nil)
	}

	// Get machine stats if requested
	var machineStats map[uuid.UUID]*model.APISiteMachineStats
	if includeMachineStats {
		machineStats, err = common.GetSiteMachineCountStats(ctx, nil, gash.dbSession, logger, &provider.ID, nil)
		if err != nil {
			logger.Error().Err(err).Msg("unable to request Machine stats for Site")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Unable to request Machine stats for Site", nil)
		}
	}

	// Get GPU stats if requested
	var gpuStats map[uuid.UUID][]model.APIMachineGPUStats
	if includeGpuStats {
		gpuStats, err = common.GetSiteGPUStats(ctx, nil, gash.dbSession, logger, &provider.ID, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving GPU stats by site")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Error retrieving GPU stats by site", nil)
		}
	}

	stDAO := cdbm.NewSiteDAO(gash.dbSession)

	siteIDs := mapset.NewSet[uuid.UUID]()

	tsMap := map[uuid.UUID]*cdbm.TenantSite{}

	if provider != nil {
		// Retrieve Sites from Provider perspective
		sites, _, serr := stDAO.GetAll(ctx, nil, cdbm.SiteFilterInput{InfrastructureProviderIDs: []uuid.UUID{provider.ID}}, paginator.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if serr != nil {
			logger.Error().Err(serr).Msg("error retrieving all Sites by param from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Sites", nil)
		}
		for _, site := range sites {
			siteIDs.Add(site.ID)
		}
	}

	if tenant != nil {
		// Get Sites from Tenant perspective
		tsDAO := cdbm.NewTenantSiteDAO(gash.dbSession)
		tss, _, serr := tsDAO.GetAll(ctx, nil, cdbm.TenantSiteFilterInput{TenantIDs: []uuid.UUID{tenant.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if serr != nil {
			logger.Error().Err(serr).Msg("error retrieving Tenant Site associations from DB by Tenant ID")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site associated with Tenant", nil)
		}

		for _, ts := range tss {
			siteIDs.Add(ts.SiteID)
			tsMap[ts.SiteID] = &ts
		}

		// Add the Sites where the Tenant has effective TargetedInstanceCreation,
		// honoring per-site TenantSite.config overrides.
		privilegedSiteIDs, serr := common.GetPrivilegedAccessSiteIDsForTenant(ctx, nil, gash.dbSession, tenant)
		if serr != nil {
			logger.Error().Err(serr).Msg("error resolving privileged Site access for Tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to resolve Tenant capability, DB error", nil)
		}
		for _, siteID := range privilegedSiteIDs {
			siteIDs.Add(siteID)
		}
	}

	filter.SiteIDs = siteIDs.ToSlice()

	// Get Sites from DB
	sites, total, err := stDAO.GetAll(ctx, nil, filter, paginator.PageInput{Offset: pageRequest.Offset, Limit: pageRequest.Limit, OrderBy: pageRequest.OrderBy}, qIncludeRelations)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Sites from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Sites, DB error", nil)
	}

	// Get status details
	sdDAO := cdbm.NewStatusDetailDAO(gash.dbSession)

	pagedSiteIDs := []string{}
	for _, site := range sites {
		pagedSiteIDs = append(pagedSiteIDs, site.ID.String())
	}

	// Get status details for the paged Sites
	ssds, serr := sdDAO.GetRecentByEntityIDs(ctx, nil, pagedSiteIDs, common.RECENT_STATUS_DETAIL_COUNT)
	if serr != nil {
		logger.Warn().Err(serr).Msg("error retrieving Status Details for Sites from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate status history for Sites", nil)
	}
	ssdMap := map[string][]cdbm.StatusDetail{}
	for _, ssd := range ssds {
		cssd := ssd
		ssdMap[ssd.EntityID] = append(ssdMap[ssd.EntityID], cssd)
	}

	// Create response
	apiSites := []model.APISite{}
	for _, site := range sites {
		apiSite := model.NewAPISite(site, ssdMap[site.ID.String()], tsMap[site.ID])
		// Attach machine stats for the site if they exist.
		apiSite.MachineStats = machineStats[site.ID]
		// Attach GPU stats when requested. Sites with no GPUs get an empty
		// array (not null), matching the single-site GPU stats endpoint.
		if includeGpuStats {
			siteGpuStats := gpuStats[site.ID]
			if siteGpuStats == nil {
				siteGpuStats = []model.APIMachineGPUStats{}
			}
			apiSite.GpuStats = &siteGpuStats
		}
		apiSites = append(apiSites, apiSite)
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

	return c.JSON(http.StatusOK, apiSites)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteSiteHandler is the API Handler for deleting a Site
type DeleteSiteHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewDeleteSiteHandler initializes and returns a new handler for deleting Site
func NewDeleteSiteHandler(dbSession *cdb.Session, tc tClient.Client, cfg *config.Config) DeleteSiteHandler {
	return DeleteSiteHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Delete a Site
// @Description Delete a Site owned by the infrastructure provider
// @Tags site
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Site"
// @Param purgeMachines query boolean false "Permanently remove all Machines associated with this Site"
// @Success 204
// @Router /v2/org/{org}/nico/site/{id} [delete]
func (dsh DeleteSiteHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Site", "Delete", c, dsh.tracerSpan)
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

	// Validate role, only Provider Admins are allowed to delete Site
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Site ID from URL param
	stStrID := c.Param("id")

	dsh.tracerSpan.SetAttribute(handlerSpan, attribute.String("site_id", stStrID), logger)

	stID, err := uuid.Parse(stStrID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Site ID in URL", nil)
	}

	// Check query param for purging Machine data
	purgeMachinesStr := c.QueryParam("purgeMachines")
	purgeMachines := false
	if purgeMachinesStr != "" {
		purgeMachines, err = strconv.ParseBool(purgeMachinesStr)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid purgeMachines query param", nil)
		}
	}

	// Check if the org has access to the Site
	// Check if org has an Infrastructure Provider
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, dsh.dbSession, org)
	if err != nil {
		if err == common.ErrOrgInstrastructureProviderNotFound {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "No Infrastructure Provider found for this org", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Infrastructure Provider for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Infrastructure Provider", nil)
	}

	// Get Site
	stDAO := cdbm.NewSiteDAO(dsh.dbSession)
	st, err := stDAO.GetByID(ctx, nil, stID, nil, false)
	if err != nil {
		logger.Warn().Err(err).Msg("error retrieving Site from DB by ID")
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not retrieve Site with ID in URL", nil)
	}

	if st.InfrastructureProviderID != ip.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site does not belong to org's Infrastructure Provider", nil)
	}

	// Check for Allocations for Site
	aDAO := cdbm.NewAllocationDAO(dsh.dbSession)
	allocationFilter := cdbm.AllocationFilterInput{SiteIDs: []uuid.UUID{st.ID}}
	atotal, err := aDAO.GetCount(ctx, nil, allocationFilter)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Allocations count for Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Allocations count for Site", nil)
	}

	if atotal > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site has Allocations which must be deleted first", nil)
	}

	err = cdb.WithTx(ctx, dsh.dbSession, func(tx *cdb.Tx) error {
		// Delete Site
		derr := stDAO.Delete(ctx, tx, stID)
		if derr != nil {
			logger.Error().Err(derr).Msg("error deleting Site from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Site, error deleting Site in data store", nil)
		}

		// Delete Temporal namespace
		tosc := dsh.tc.OperatorService()
		_, derr = tosc.DeleteNamespace(ctx, &tOperatorv1.DeleteNamespaceRequest{
			Namespace: st.ID.String(),
		})
		if derr != nil {
			if strings.Contains(derr.Error(), "not found") {
				logger.Warn().Str("Site ID", stStrID).Msg("temporal namespace not found, continuing")
			} else {
				logger.Error().Err(derr).Msg("error deleting Temporal namespace for Site")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Site, error deleting workflow namespace", nil)
			}
		}

		if dsh.cfg.GetSiteManagerEnabled() {
			url := dsh.cfg.GetSiteManagerEndpoint()
			derr = csm.DeleteSite(ctx, logger, st.ID.String(), url)
			if derr == csm.ErrSiteNotFound {
				logger.Warn().Err(derr).Msg("Site not found in Site Manager, continuing with deletion")
			} else if derr != nil {
				logger.Error().Err(derr).Msg("error deleting site in site manager")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Site, error deleting Site Manager entry", nil)
			}
		}

		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to delete Site, DB transaction error")
	}

	// Trigger Cloud workflow to delete Site Component from DB
	wid, err := siteWorkflow.ExecuteDeleteSiteComponentsWorkflow(ctx, dsh.tc, st.ID, st.InfrastructureProviderID, purgeMachines)
	if err != nil {
		logger.Error().Err(err).Msg("failed to execute delete Site Components workflow")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to initiate Site Component deletion from DB", nil)
	}

	logger.Info().Str("Workflow ID", *wid).Msg("triggered delete Site Component workflow")

	// Create response
	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusAccepted, model.NewAPIDeletionAcceptedResponse())
}

// GetSiteStatusDetailsHandler is the API Handler for getting Site StatusDetail records
type GetSiteStatusDetailsHandler struct {
	dbSession  *cdb.Session
	tracerSpan *cutil.TracerSpan
}

// NewGetSiteStatusDetailsHandler initializes and returns a new handler to retrieve Site StatusDetail records
func NewGetSiteStatusDetailsHandler(dbSession *cdb.Session) GetSiteStatusDetailsHandler {
	return GetSiteStatusDetailsHandler{
		dbSession:  dbSession,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get Site StatusDetails
// @Description Get all StatusDetails for Site
// @Tags Site
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Site"
// @Success 200 {object} []model.APIStatusDetail
// @Router /v2/org/{org}/nico/Site/{id}/status-history [get]
func (gssdh GetSiteStatusDetailsHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Site", "Get", c, gssdh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	provider, tenant, apiErr := common.IsProviderOrTenant(ctx, logger, gssdh.dbSession, org, dbUser, true, nil)
	if apiErr != nil {
		return c.JSON(apiErr.Code, apiErr)
	}

	// Get Site ID from URL param
	stStrID := c.Param("id")
	gssdh.tracerSpan.SetAttribute(handlerSpan, attribute.String("site_id", stStrID), logger)
	stID, err := uuid.Parse(stStrID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Site ID in URL", nil)
	}

	// Get Site
	stDAO := cdbm.NewSiteDAO(gssdh.dbSession)
	st, err := stDAO.GetByID(ctx, nil, stID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Site with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site", nil)
	}

	// Check Site association
	isAssociated := false

	if provider != nil {
		isAssociated = provider.ID == st.InfrastructureProviderID
	}

	if !isAssociated && tenant != nil {
		tsDAO := cdbm.NewTenantSiteDAO(gssdh.dbSession)

		_, tsCount, serr := tsDAO.GetAll(ctx, nil, cdbm.TenantSiteFilterInput{TenantIDs: []uuid.UUID{tenant.ID}, SiteIDs: []uuid.UUID{stID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if serr != nil {
			logger.Error().Err(serr).Msg("error retrieving TenantSite from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant/Site association to determine access to Site, DB error", nil)
		}
		if tsCount > 0 {
			isAssociated = true
		}

		if !isAssociated {
			enabled, serr := common.TenantHasTargetedInstanceCreation(ctx, nil, gssdh.dbSession, tenant, &common.TenantPrivilegeScope{SiteID: &st.ID})
			if serr != nil {
				logger.Error().Err(serr).Msg("error resolving TargetedInstanceCreation for Tenant/Site")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to resolve Tenant capability for Site, DB error", nil)
			}
			isAssociated = enabled
		}
	}

	if !isAssociated {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site is not associated with org", nil)
	}

	// handle retrieving and building status details response
	apiSds, err := handleEntityStatusDetails(ctx, c, gssdh.dbSession, stStrID, logger)
	if err != nil {
		return err
	}

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiSds)
}
