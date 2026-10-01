// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"

	mapset "github.com/deckarep/golang-set/v2"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"go.opentelemetry.io/otel/attribute"
	tclient "go.temporal.io/sdk/client"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
)

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllIpxeTemplateHandler is the API Handler for getting all iPXE templates
type GetAllIpxeTemplateHandler struct {
	dbSession *cdb.Session
	tc        tclient.Client
	cfg       *config.Config
}

// NewGetAllIpxeTemplateHandler initializes and returns a new handler for getting all iPXE templates
func NewGetAllIpxeTemplateHandler(dbSession *cdb.Session, tc tclient.Client, cfg *config.Config) GetAllIpxeTemplateHandler {
	return GetAllIpxeTemplateHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get all iPXE templates
// @Description Get all iPXE templates propagated from nico-core. Templates are global (one row per stable core template UUID); per-site availability is recorded internally. The `siteId` query parameter is optional and may be repeated to restrict results to templates available at one or more sites. When omitted, a Provider Admin/Viewer receives templates available at any site owned by their infrastructure provider; a Tenant Admin receives templates available at any site whose provider the tenant has a Tenant Account on.
// @Tags iPXE Template
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query []string false "Optional site ID(s); may be repeated to restrict results to templates available at any of the sites"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APIIpxeTemplate
// @Router /v2/org/{org}/nico/ipxe-template [get]
func (h GetAllIpxeTemplateHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("IpxeTemplate", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate role (Provider Admin/Viewer or Tenant Admin) and org membership
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, h.dbSession, org, dbUser, true, nil)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Parse optional siteId query parameters. Multiple values (repeated
	// `?siteId=...&siteId=...`) are supported.
	if err := common.ValidateKnownQueryParams(c.QueryParams(), model.APIIpxeTemplateGetAllRequest{}, pagination.PageRequest{}); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	requestedSiteIDStrs := c.QueryParams()["siteId"]
	requestedSiteIDs := make([]uuid.UUID, 0, len(requestedSiteIDStrs))
	for _, s := range requestedSiteIDStrs {
		if s == "" {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid siteId in query parameter: empty value", nil)
		}
		parsed, perr := uuid.Parse(s)
		if perr != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid siteId in query parameter: %s", s), nil)
		}
		requestedSiteIDs = append(requestedSiteIDs, parsed)
	}

	// Build the caller's authorized site set, tracking which sites come from the
	// provider path vs the tenant path. A site can be in both sets for a
	// dual-role caller — provider access wins (fewer restrictions).
	providerSites := mapset.NewSet[uuid.UUID]()
	tenantSites := mapset.NewSet[uuid.UUID]()

	if infrastructureProvider != nil {
		siteDAO := cdbm.NewSiteDAO(h.dbSession)
		sites, _, serr := siteDAO.GetAll(ctx, nil,
			cdbm.SiteFilterInput{InfrastructureProviderIDs: []uuid.UUID{infrastructureProvider.ID}},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		if serr != nil {
			logger.Error().Err(serr).Msg("error retrieving provider sites from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve provider sites, DB error", nil)
		}
		for i := range sites {
			providerSites.Add(sites[i].ID)
		}
	}

	if tenant != nil {
		tsDAO := cdbm.NewTenantSiteDAO(h.dbSession)
		tss, _, terr := tsDAO.GetAll(ctx, nil,
			cdbm.TenantSiteFilterInput{TenantIDs: []uuid.UUID{tenant.ID}},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		if terr != nil {
			logger.Error().Err(terr).Msg("error retrieving Tenant Site associations from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant Site associations, DB error", nil)
		}
		for i := range tss {
			tenantSites.Add(tss[i].SiteID)
		}
	}

	isAuthorized := func(id uuid.UUID) bool {
		return providerSites.Contains(id) || tenantSites.Contains(id)
	}

	// Determine the effective site filter:
	//   - siteId(s) provided: must all be authorized; use them as-is.
	//   - siteId(s) omitted:  use the union of provider and tenant accessible sites.
	var effectiveSiteIDs []uuid.UUID
	if len(requestedSiteIDs) > 0 {
		for _, id := range requestedSiteIDs {
			if !isAuthorized(id) {
				logger.Warn().Str("siteID", id.String()).Msg("org not authorized to access requested Site")
				return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Current org is not authorized to access Site: %s", id.String()), nil)
			}
		}
		effectiveSiteIDs = requestedSiteIDs
	} else {
		effectiveSiteIDs = providerSites.Union(tenantSites).ToSlice()
	}

	// No authorized sites — neither provider-owned nor reachable via a tenant account.
	if len(effectiveSiteIDs) == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with any Site", nil)
	}

	// Validate pagination request
	pageRequest := pagination.PageRequest{}
	if err := c.Bind(&pageRequest); err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}
	if err := pageRequest.Validate(cdbm.IpxeTemplateOrderByFields); err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Resolve which template IDs are available at the authorized sites via
	// the IpxeTemplateSiteAssociation table.
	itsaDAO := cdbm.NewIpxeTemplateSiteAssociationDAO(h.dbSession)
	associations, _, err := itsaDAO.GetAll(ctx, nil,
		cdbm.IpxeTemplateSiteAssociationFilterInput{SiteIDs: effectiveSiteIDs},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving iPXE template site associations from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve iPXE template site associations, DB error", nil)
	}

	templateIDSet := mapset.NewSet[uuid.UUID]()
	for _, a := range associations {
		templateIDSet.Add(a.IpxeTemplateID)
	}
	templateIDs := templateIDSet.ToSlice()

	templateDAO := cdbm.NewIpxeTemplateDAO(h.dbSession)
	templates, total, err := templateDAO.GetAll(
		ctx,
		nil,
		cdbm.IpxeTemplateFilterInput{IpxeTemplateIDs: templateIDs},
		cdbp.PageInput{
			Offset:  pageRequest.Offset,
			Limit:   pageRequest.Limit,
			OrderBy: pageRequest.OrderBy,
		},
	)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving iPXE templates from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve iPXE templates, DB error", nil)
	}

	apiTemplates := []*model.APIIpxeTemplate{}
	for i := range templates {
		apiTemplates = append(apiTemplates, model.NewAPIIpxeTemplate(&templates[i]))
	}

	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiTemplates)
}

// ~~~~~ Get Handler ~~~~~ //

// GetIpxeTemplateHandler is the API Handler for retrieving a single iPXE template
type GetIpxeTemplateHandler struct {
	dbSession *cdb.Session
	tc        tclient.Client
	cfg       *config.Config
}

// NewGetIpxeTemplateHandler initializes and returns a new handler to retrieve an iPXE template
func NewGetIpxeTemplateHandler(dbSession *cdb.Session, tc tclient.Client, cfg *config.Config) GetIpxeTemplateHandler {
	return GetIpxeTemplateHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Retrieve an iPXE template
// @Description Retrieve an iPXE template by its stable core ID. The caller must be authorized for at least one Site at which the template is currently available (Provider Admin/Viewer for a Site owned by their infrastructure provider, or Tenant Admin with a Tenant Account on a Site reporting the template).
// @Tags iPXE Template
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "Stable template ID (UUID from core)"
// @Success 200 {object} model.APIIpxeTemplate
// @Router /v2/org/{org}/nico/ipxe-template/{id} [get]
func (h GetIpxeTemplateHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("IpxeTemplate", "Get", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate role (Provider Admin/Viewer or Tenant Admin) — this also validates
	// org membership, so no separate membership check is needed here.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, h.dbSession, org, dbUser, true, nil)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Parse template ID from URL (this is the stable core template UUID, which is
	// also the primary key in REST).
	templateIDStr := c.Param("id")
	templateID, err := uuid.Parse(templateIDStr)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid iPXE template ID: %s", templateIDStr), nil)
	}

	logger = logger.With().Str("IpxeTemplate ID", templateIDStr).Logger()
	cotel.SetAttribute(handlerSpan, attribute.String("ipxe_template_id", templateIDStr))

	templateDAO := cdbm.NewIpxeTemplateDAO(h.dbSession)
	tmpl, err := templateDAO.Get(ctx, nil, templateID)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find iPXE template with ID: %s", templateIDStr), nil)
		}
		logger.Error().Err(err).Msg("error retrieving iPXE template from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve iPXE template, DB error", nil)
	}

	// Authorization: caller must be associated (via provider ownership or tenant
	// account) with at least one Site at which this template is currently
	// reported.
	itsaDAO := cdbm.NewIpxeTemplateSiteAssociationDAO(h.dbSession)
	associations, _, err := itsaDAO.GetAll(ctx, nil,
		cdbm.IpxeTemplateSiteAssociationFilterInput{IpxeTemplateIDs: []uuid.UUID{templateID}},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving iPXE template site associations")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to verify iPXE template authorization, DB error", nil)
	}

	hasAccess, err := callerHasAccessToAnyAssociatedSite(ctx, logger, h.dbSession, infrastructureProvider, tenant, associations)
	if err != nil {
		logger.Error().Err(err).Msg("error verifying iPXE template site authorization")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to verify iPXE template authorization, DB error", nil)
	}
	if !hasAccess {
		logger.Warn().Msg("caller is not authorized to access any Site associated with this iPXE template")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not authorized to access this iPXE template", nil)
	}

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIIpxeTemplate(tmpl))
}

// callerHasAccessToAnyAssociatedSite reports whether the caller (provider or
// tenant) has access to at least one site in the given association set. A non-nil
// error indicates a lookup failure and must be surfaced by callers as HTTP 500,
// rather than being collapsed into a "no access" (HTTP 403) answer.
func callerHasAccessToAnyAssociatedSite(
	ctx context.Context,
	logger zerolog.Logger,
	dbSession *cdb.Session,
	provider *cdbm.InfrastructureProvider,
	tenant *cdbm.Tenant,
	associations []cdbm.IpxeTemplateSiteAssociation,
) (bool, error) {
	if len(associations) == 0 {
		return false, nil
	}

	siteIDs := make([]uuid.UUID, 0, len(associations))
	for _, a := range associations {
		siteIDs = append(siteIDs, a.SiteID)
	}

	// Provider path: any site owned by the caller's provider.
	if provider != nil {
		siteDAO := cdbm.NewSiteDAO(dbSession)
		sites, _, serr := siteDAO.GetAll(ctx, nil, cdbm.SiteFilterInput{
			InfrastructureProviderIDs: []uuid.UUID{provider.ID},
			SiteIDs:                   siteIDs,
		}, cdbp.PageInput{Limit: cutil.GetPtr(1)}, nil)
		if serr != nil {
			logger.Error().Err(serr).Msg("error retrieving provider sites for iPXE template authorization")
			return false, serr
		}
		if len(sites) > 0 {
			return true, nil
		}
	}

	// Tenant path: any site reachable via a TenantSite association.
	if tenant != nil {
		tsDAO := cdbm.NewTenantSiteDAO(dbSession)
		tss, _, terr := tsDAO.GetAll(ctx, nil, cdbm.TenantSiteFilterInput{
			TenantIDs: []uuid.UUID{tenant.ID},
			SiteIDs:   siteIDs,
		}, cdbp.PageInput{Limit: cutil.GetPtr(1)}, nil)
		if terr != nil {
			logger.Error().Err(terr).Msg("error retrieving Tenant Site associations for iPXE template authorization")
			return false, terr
		}
		if len(tss) > 0 {
			return true, nil
		}
	}

	return false, nil
}
