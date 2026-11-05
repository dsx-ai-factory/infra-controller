// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package api

import (
	"net/http"
	"time"

	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/site-manager/internal/certmanager"
	apiHandler "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/api/handler"
	crdclientv1 "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/client/clientset/versioned/typed/crds/v1"
)

// Names of the Site routes. Clients resolve their paths by name with
// NewSiteRouteResolver.
const (
	CreateSiteRouteName      = "createSite"
	GetSiteRouteName         = "getSite"
	DeleteSiteRouteName      = "deleteSite"
	RollSiteRouteName        = "rollSite"
	RegisterSiteRouteName    = "registerSite"
	CreateSiteCredsRouteName = "createSiteCreds"
)

// NewSystemRoutes returns the health, version and metrics routes.
func NewSystemRoutes(version string) []Route {
	return []Route{
		{
			Path:    "/healthz",
			Method:  http.MethodGet,
			Handler: apiHandler.NewHealthCheckHandler(),
		},
		{
			Path:    "/version",
			Method:  http.MethodGet,
			Handler: apiHandler.NewVersionHandler(version),
		},
		{
			Path:    "/metrics",
			Method:  http.MethodGet,
			Handler: apiHandler.NewMetricsHandler(),
		},
	}
}

// NewSiteRoutes returns the routes that manage Sites in sites and hand out
// their credentials. OTPs expire after otpDuration.
func NewSiteRoutes(sites crdclientv1.SiteInterface, otpDuration time.Duration, certManager *certmanager.Client) []Route {
	return []Route{
		{
			Name:    CreateSiteRouteName,
			Path:    "/v1/site",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateSiteHandler(sites, otpDuration),
		},
		{
			Name:    GetSiteRouteName,
			Path:    "/v1/site/:uuid",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetSiteHandler(sites),
		},
		{
			Name:    DeleteSiteRouteName,
			Path:    "/v1/site/:uuid",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteSiteHandler(sites),
		},
		{
			Name:    RollSiteRouteName,
			Path:    "/v1/site/roll/:uuid",
			Method:  http.MethodPost,
			Handler: apiHandler.NewRollSiteHandler(sites, otpDuration),
		},
		{
			Name:    RegisterSiteRouteName,
			Path:    "/v1/site/register/:uuid",
			Method:  http.MethodPost,
			Handler: apiHandler.NewRegisterSiteHandler(sites),
		},
		{
			Name:    CreateSiteCredsRouteName,
			Path:    "/v1/sitecreds",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateSiteCredsHandler(sites, certManager),
		},
	}
}

// NewSiteRouteResolver returns an Echo instance with the Site routes
// registered by name. Clients resolve a Site route's path with its Reverse
// method. The resolver never runs the handlers, so they're built without
// their dependencies.
func NewSiteRouteResolver() *echo.Echo {
	e := echo.New()
	for _, route := range NewSiteRoutes(nil, 0, nil) {
		e.Add(route.Method, route.Path, route.Handler.Handle).Name = route.Name
	}
	return e
}
