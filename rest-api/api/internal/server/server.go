// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package server

import (
	"fmt"
	"net/http"
	"os"
	"strings"
	"time"

	handler "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/api"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/middleware"
	cconfig "github.com/NVIDIA/infra-controller/rest-api/common/pkg/config"
	cerr "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"

	"github.com/getsentry/sentry-go"
	sentryZerolog "github.com/getsentry/sentry-go/zerolog"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	zlogadapter "logur.dev/adapter/zerolog"
	"logur.dev/logur"

	echoPrometheus "github.com/labstack/echo-contrib/echoprometheus"
	echo "github.com/labstack/echo/v4"
	echoMiddleware "github.com/labstack/echo/v4/middleware"

	tsdkClient "go.temporal.io/sdk/client"
	tsdkConverter "go.temporal.io/sdk/converter"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api"
	dpsclient "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/dps"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"

	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authn "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authentication"
	otprop "go.opentelemetry.io/contrib/propagators/ot"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/propagation"
	"go.temporal.io/sdk/contrib/opentelemetry"
	"go.temporal.io/sdk/interceptor"
	"golang.org/x/time/rate"

	// Imports for API doc generation
	_ "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
)

const (
	// ReadTimeout bounds how long the server spends reading a request.
	ReadTimeout = 60 * time.Second

	// WriteTimeout is the deadline for writing a response, applied to the
	// connection when the request arrives. It is the hard ceiling on total
	// handler latency: a handler that produces its response after this elapses
	// cannot deliver it, so the client sees a dropped connection instead of the
	// status the handler chose. Any handler-side wait, including the Temporal
	// proxy timeout ladders, must complete well inside it.
	WriteTimeout = 60 * time.Second
)

func InitTemporalClients(tcfg *cconfig.TemporalConfig, tracingEnabled bool) (tsdkClient.Client, tsdkClient.NamespaceClient, error) {
	var tc tsdkClient.Client
	var tnc tsdkClient.NamespaceClient

	tLogger := logur.LoggerToKV(zlogadapter.New(zerolog.New(os.Stderr)))

	var (
		tInterceptors []interceptor.ClientInterceptor
		err           error
	)

	if tracingEnabled {
		otelInterceptor, serr := opentelemetry.NewTracingInterceptor(opentelemetry.TracerOptions{TextMapPropagator: otel.GetTextMapPropagator()})
		if serr != nil {
			log.Panic().Err(serr).Msg("unable to get otelInterceptor")
		}
		tInterceptors = append(tInterceptors, otelInterceptor)
	}

	tOptions := tsdkClient.Options{
		HostPort: fmt.Sprintf("%v:%v", tcfg.Host, tcfg.Port),
		// This client connects to `cloud` namespace
		Namespace: tcfg.Namespace,
		ConnectionOptions: tsdkClient.ConnectionOptions{
			TLS: tcfg.ClientTLSCfg,
		},
		DataConverter: tsdkConverter.NewCompositeDataConverter(
			tsdkConverter.NewNilPayloadConverter(),
			tsdkConverter.NewByteSlicePayloadConverter(),
			tsdkConverter.NewProtoJSONPayloadConverterWithOptions(tsdkConverter.ProtoJSONPayloadConverterOptions{
				AllowUnknownFields: true,
			}),
			tsdkConverter.NewProtoPayloadConverter(),
			tsdkConverter.NewJSONPayloadConverter(),
		),
		Interceptors: tInterceptors,
		Logger:       tLogger,
	}

	log.Info().Msg("creating Temporal client")
	tc, err = tsdkClient.NewLazyClient(tOptions)
	if err != nil {
		log.Error().Err(err).Msg("failed to create Temporal client")
		return nil, nil, err
	}

	log.Info().Msg("creating Temporal namespace client")
	tnc, err = tsdkClient.NewNamespaceClient(tOptions)
	if err != nil {
		log.Error().Err(err).Msg("failed to create Temporal Namespace client")
		return nil, nil, err
	}

	return tc, tnc, err
}

func InitAPIServer(cfg *config.Config, dbSession *cdb.Session, tc tsdkClient.Client, tnc tsdkClient.NamespaceClient, scp *sc.ClientPool, dps dpsclient.PowerProvisioner) *echo.Echo {
	e := echo.New()
	e.HideBanner = true
	e.HTTPErrorHandler = cerr.DefaultHTTPErrorHandler

	// Add timeouts to prevent SLOWLORIS attacks
	e.Server.ReadHeaderTimeout = 3 * time.Second
	e.Server.ReadTimeout = ReadTimeout
	e.Server.WriteTimeout = WriteTimeout
	e.Server.IdleTimeout = 120 * time.Second

	// Add middleware to set the API name
	e.Use(middleware.APIName(cfg.GetAPIName()))

	// General middlewares
	e.Use(echoMiddleware.Recover())

	// Custom middlewares
	e.Use(middleware.CORS())

	// Secure middleware configures echo with secure headers
	e.Use(middleware.Secure())

	// Limit request body size to prevent OOM from oversized payloads
	e.Use(echoMiddleware.BodyLimit("10M"))

	// Rate limiter middleware (if enabled)
	rateLimiterConfig := cfg.GetRateLimiterConfig()
	if rateLimiterConfig.Enabled {
		log.Info().
			Float64("Rate", rateLimiterConfig.Rate).
			Int("Burst", rateLimiterConfig.Burst).
			Int("Expires In", rateLimiterConfig.ExpiresIn).
			Msg("Rate limiter enabled")

		rateLimiterMiddlewareConfig := echoMiddleware.RateLimiterConfig{
			Skipper: echoMiddleware.DefaultSkipper,
			Store: echoMiddleware.NewRateLimiterMemoryStoreWithConfig(
				echoMiddleware.RateLimiterMemoryStoreConfig{
					Rate:      rate.Limit(rateLimiterConfig.Rate),
					Burst:     rateLimiterConfig.Burst,
					ExpiresIn: time.Duration(rateLimiterConfig.ExpiresIn) * time.Second,
				},
			),
			IdentifierExtractor: func(ctx echo.Context) (string, error) {
				id := ctx.RealIP()
				return id, nil
			},
			ErrorHandler: func(context echo.Context, err error) error {
				log.Warn().
					Err(err).
					Str("ip", context.RealIP()).
					Str("path", context.Request().URL.Path).
					Msg("Rate limiter extractor error")
				return cerr.NewAPIErrorResponse(context, http.StatusForbidden, "Failed to extract rate limiting identifier from request", nil)
			},
			DenyHandler: func(context echo.Context, identifier string, err error) error {
				log.Warn().
					Str("identifier", identifier).
					Str("path", context.Request().URL.Path).
					Msg("Rate limit exceeded")
				return cerr.NewAPIErrorResponse(context, http.StatusTooManyRequests, "Request rate limit exceeded, please re-evaluate your request patterns", nil)
			},
		}

		e.Use(echoMiddleware.RateLimiterWithConfig(rateLimiterMiddlewareConfig))
	} else {
		log.Info().Msg("Rate limiter disabled")
	}

	if cfg.GetTracingEnabled() {
		svcName := cfg.GetTracingServiceName()
		if svcName != "" {
			// Composite: WithPropagators replaces, so OT alone dropped W3C.
			e.Use(otelecho.Middleware(svcName,
				otelecho.WithSkipper(skipTracingRoutes),
				otelecho.WithPropagators(propagation.NewCompositeTextMapPropagator(
					propagation.TraceContext{},
					propagation.Baggage{},
					otprop.OT{},
				)),
			))
		} else {
			log.Warn().Msg("failed to get Tracing Service Name, skipping OTel middleware")
		}
	}

	// Sentry middleware
	sentryDSN := cfg.GetSentryDSN()
	if sentryDSN != "" {
		// Initialize Sentry
		err := sentry.Init(sentry.ClientOptions{
			Dsn: sentryDSN,
			BeforeSend: func(event *sentry.Event, hint *sentry.EventHint) *sentry.Event {
				// Modify or filter events before sending them to Sentry
				return event
			},
			Debug:            true,
			AttachStacktrace: true,
		})
		if err != nil {
			log.Error().Err(err).Msg("Sentry initialization failed")
		} else {
			defer sentry.Flush(2 * time.Second)

			// Configure Zerolog to use Sentry as a writer
			sentryWriter, err := sentryZerolog.New(sentryZerolog.Config{
				ClientOptions: sentry.ClientOptions{
					Dsn: sentryDSN,
				},
				Options: sentryZerolog.Options{
					Levels:          []zerolog.Level{zerolog.ErrorLevel, zerolog.FatalLevel, zerolog.PanicLevel},
					WithBreadcrumbs: true,
					FlushTimeout:    3 * time.Second,
				},
			})
			if err != nil {
				log.Error().Err(err).Msg("failed to create Sentry writer")
			} else {
				defer sentryWriter.Close()

				// Use Sentry writer in Zerolog
				log.Logger = zerolog.New(zerolog.MultiLevelWriter(os.Stderr, sentryWriter))
			}
		}
	} else {
		log.Warn().Msg("Sentry DSN is not configured, skipping Sentry middleware")
	}

	// Routes
	// Common routes are service oriented e.g. health
	commonAPIRoutes := api.NewSystemAPIRoutes()
	for _, commonAPIRoute := range commonAPIRoutes {
		// Register route
		e.Add(commonAPIRoute.Method, commonAPIRoute.Path, commonAPIRoute.Handler.Handle)
	}

	// Public .well-known/* tenant-identity routes — no auth, JWT verifiers
	wellKnownRoutes := api.NewWellKnownRoutes(dbSession, scp, cfg)
	versionPrefix := "/" + cfg.GetAPIRouteVersion()
	for _, r := range wellKnownRoutes {
		e.Add(r.Method, versionPrefix+r.Path, r.Handler.Handle)
	}

	// Versioned routes
	// Add middlewares for versioned group
	routeGroup := e.Group("/" + cfg.GetAPIRouteVersion())
	// Add logging
	routeGroup.Use(middleware.Logger())
	// Add middleware to handle unmatched routes, must be added before audit or auth middleware
	routeGroup.Use(middleware.NotFoundHandler(cfg))

	// Add middleware to use for audit
	if noAudit := os.Getenv("NO_AUDIT"); !strings.EqualFold(noAudit, "true") {
		routeGroup.Use(middleware.AuditBody(dbSession))
		routeGroup.Use(middleware.AuditLog(dbSession))
	}

	jwtOriginConfig := cfg.GetOrInitJWTOriginConfig()
	if jwtOriginConfig == nil {
		log.Panic().Msg("JWT origin config not initialized, cannot initialize auth middleware")
	}

	keycloakConfig, _ := cfg.GetOrInitKeycloakConfig()
	payloadEncryptionConfig := cconfig.NewPayloadEncryptionConfig(cfg.GetTemporalEncryptionKey())

	// Wrap the auth middleware to check readiness (optional, can be removed if panic is sufficient)
	authMiddleware := authn.Auth(dbSession, tc, jwtOriginConfig, payloadEncryptionConfig, keycloakConfig)
	routeGroup.Use(authMiddleware)

	apiRoutes := api.NewAPIRoutes(dbSession, tc, tnc, scp, cfg, dps)
	for _, apiRoute := range apiRoutes {
		routeGroup.Add(apiRoute.Method, apiRoute.Path, apiRoute.Handler.Handle)
	}
	if keycloakConfig != nil {
		log.Info().Msg("Registering Keycloak auth routes")
		authGroup := e.Group("/auth")
		authAPIRoutes := handler.NewAuthRoutes(keycloakConfig)
		for _, authnzAPIRoute := range authAPIRoutes {
			log.Info().Str("method", authnzAPIRoute.Method).Str("path", authnzAPIRoute.Path).Msg("Registering auth route")
			authGroup.Add(authnzAPIRoute.Method, authnzAPIRoute.Path, authnzAPIRoute.Handler.Handle)
		}
		log.Info().Msg("Keycloak auth routes registered successfully")
	} else {
		log.Warn().Msg("Keycloak not configured, skipping auth routes registration")
	}

	return e
}

func InitMetricsServer(e *echo.Echo, namespace string) *echo.Echo {
	ep := echo.New()
	ep.HideBanner = true

	conf := echoPrometheus.MiddlewareConfig{
		// The prefix has to go in Subsystem, since echoprometheus substitutes
		// its own "echo" for an empty one.
		Subsystem: namespace,
		Skipper:   api.MetricsURLSkipper,
	}

	p := echoPrometheus.NewMiddlewareWithConfig(conf)
	// Scrape metrics from Main Server
	e.Use(p)
	// Setup metrics endpoint at another server
	ep.GET("/metrics", echoPrometheus.NewHandler())

	return ep
}

// skipTracingRoutes returns true if the route should be skipped for tracing
func skipTracingRoutes(c echo.Context) bool {
	return api.IsSystemRoute(c.Path())
}
