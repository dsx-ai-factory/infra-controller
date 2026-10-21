// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package server

import (
	"bytes"
	"context"
	"fmt"
	"net"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	_ "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cconfig "github.com/NVIDIA/infra-controller/rest-api/common/pkg/config"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	echo "github.com/labstack/echo/v4"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.opentelemetry.io/otel"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.opentelemetry.io/otel/sdk/trace/tracetest"
	"go.opentelemetry.io/otel/trace"
	temporalClient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/grpc"
	"google.golang.org/grpc/health"
	healthpb "google.golang.org/grpc/health/grpc_health_v1"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
)

// Test_ProxyTimeoutsFitWriteTimeout guards the ceiling that the gRPC proxy
// timeout ladder lives under. A ladder raised past WriteTimeout still looks
// self-consistent, but its timeout response is written after the connection's
// deadline and never reaches the client. The ladder is checked here rather than
// in grpcproxy because only this package knows the server deadline.
func Test_ProxyTimeoutsFitWriteTimeout(t *testing.T) {
	assert.Less(t, cutil.WorkflowContextTimeout, WriteTimeout)
	assert.Less(t, grpcproxy.WorkflowExecutionTimeout, cutil.WorkflowContextTimeout)
}

func Test_InitAPIServer(t *testing.T) {
	type args struct {
		cfg       *config.Config
		dbSession *cdb.Session
		tc        temporalClient.Client
		tnc       temporalClient.NamespaceClient
		scp       *sc.ClientPool
	}

	cfg := common.GetTestConfig()

	dbSession := cdbu.GetTestDBSession(t, true)
	defer dbSession.Close()
	// Startup syncs ConfigMap issuers into the issuer table and records history.
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Issuer)(nil), (*cdbm.StatusDetail)(nil)))
	// Registered after the session so it runs first: the enabled case starts the
	// issuer reload and JWKS loops, which must stop before the DB session closes.
	defer cfg.Close()

	tc := &tmocks.Client{}
	tnc := &tmocks.NamespaceClient{}

	tcfg, _ := cfg.GetTemporalConfig()

	scp := sc.NewClientPool(tcfg)

	t.Setenv("SENTRY_DSN", "https://bfe69b59461e44059a533274a6393155@glitchtip.test.com/3")

	// Startup fetches every configured issuer's key set, so the fixtures below
	// point at a local IdP rather than a real one.
	idp := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = w.Write([]byte(`{"keys":[{"kty":"RSA","use":"sig","kid":"test-key","alg":"RS256","n":"test-n-value","e":"AQAB"}]}`))
	}))
	defer idp.Close()

	customIssuer := []config.IssuerConfig{{
		Origin: "custom",
		Issuer: idp.URL,
		JWKS:   idp.URL + "/jwks",
	}}
	privilegedIssuer := []config.IssuerConfig{{
		Origin: "kas-legacy",
		JWKS:   idp.URL + "/jwks",
	}}

	tests := []struct {
		name               string
		keycloakEnabled    bool
		staticIssuers      []config.IssuerConfig
		wantDynamicIssuers bool
		args               args
	}{
		{
			name:               "dynamic issuer routes enabled",
			keycloakEnabled:    false,
			staticIssuers:      customIssuer,
			wantDynamicIssuers: true,
			args: args{
				cfg:       cfg,
				dbSession: dbSession,
				tc:        tc,
				tnc:       tnc,
				scp:       scp,
			},
		},
		{
			name:               "dynamic issuer routes disabled by Keycloak",
			keycloakEnabled:    true,
			staticIssuers:      customIssuer,
			wantDynamicIssuers: false,
			args: args{
				cfg:       cfg,
				dbSession: dbSession,
				tc:        tc,
				tnc:       tnc,
				scp:       scp,
			},
		},
		{
			name:               "dynamic issuer routes disabled by a privileged static origin",
			keycloakEnabled:    false,
			staticIssuers:      privilegedIssuer,
			wantDynamicIssuers: false,
			args: args{
				cfg:       cfg,
				dbSession: dbSession,
				tc:        tc,
				tnc:       tnc,
				scp:       scp,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tt.args.cfg.SetKeycloakEnabled(tt.keycloakEnabled)
			config.SetStaticIssuersForTest(t, tt.args.cfg, tt.staticIssuers)
			// Without this the route assertions below can pass vacuously: a fixture
			// that never reaches the state under test makes every expectation
			// "the route is absent", which a regression also satisfies.
			require.Equal(t, tt.wantDynamicIssuers, tt.args.cfg.DynamicIssuersEnabled(),
				"fixture must reach the policy state the routes are being checked against")

			e := InitAPIServer(tt.args.cfg, tt.args.dbSession, tt.args.tc, tt.args.tnc, tt.args.scp, nil)
			assertIssuerRoutesMatchPolicy(t, e, tt.args.cfg, tt.wantDynamicIssuers)
		})
	}
	cfg.SetKeycloakEnabled(false)
}

// assertIssuerRoutesMatchPolicy checks the route table the server actually built
// against the single condition that is supposed to govern it. The issuer routes
// and the DB-backed issuer machinery are gated together, so the registered
// surface is the observable half of that decision. wantRegistered is passed in
// rather than read from cfg, because deriving the expectation from the predicate
// under test makes a broken predicate agree with itself.
func assertIssuerRoutesMatchPolicy(t *testing.T, e *echo.Echo, cfg *config.Config, wantRegistered bool) {
	t.Helper()

	prefix := "/" + cfg.GetAPIRouteVersion() + "/org/:orgName/" + cfg.GetAPIName()
	issuerRoutes := []string{
		http.MethodPut + " " + prefix + "/auth-issuer",
		http.MethodGet + " " + prefix + "/auth-issuer",
		http.MethodGet + " " + prefix + "/auth-issuer/:authIssuerId",
		http.MethodDelete + " " + prefix + "/auth-issuer/:authIssuerId",
	}

	registered := make(map[string]bool, len(e.Routes()))
	for _, r := range e.Routes() {
		registered[r.Method+" "+r.Path] = true
	}

	for _, route := range issuerRoutes {
		assert.Equal(t, wantRegistered, registered[route], route)
	}
}

// Test_InitAPIServerTracingMiddleware proves the startup gate the tracing
// bootstrap promises: the OpenTelemetry Echo middleware is installed exactly
// when transport instrumentation is on, and a routed request then records a
// server span against the global tracer provider that the rest of the request
// nests under.
func Test_InitAPIServerTracingMiddleware(t *testing.T) {
	tests := []struct {
		descr          string
		propagators    string
		wantServerSpan bool
	}{
		{descr: "transport enabled installs the middleware", wantServerSpan: true},
		{descr: "propagation disabled skips the middleware", propagators: "none"},
	}

	cfg := common.GetTestConfig()
	dbSession := cdbu.GetTestDBSession(t, true)
	defer dbSession.Close()
	// Startup syncs ConfigMap issuers into the issuer table and records history.
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Issuer)(nil), (*cdbm.StatusDetail)(nil)))
	tcfg, _ := cfg.GetTemporalConfig()

	for _, tc := range tests {
		t.Run(tc.descr, func(t *testing.T) {
			previousProvider := otel.GetTracerProvider()
			previousPropagator := otel.GetTextMapPropagator()
			t.Cleanup(func() {
				otel.SetTracerProvider(previousProvider)
				otel.SetTextMapPropagator(previousPropagator)
			})
			exporter := tracetest.NewInMemoryExporter()
			otel.SetTracerProvider(sdktrace.NewTracerProvider(sdktrace.WithSyncer(exporter)))

			// Export stays disabled so the provider above remains global; the
			// bootstrap still decides transport instrumentation from the
			// propagator configuration.
			t.Setenv("OTEL_PROPAGATORS", tc.propagators)
			shutdown, err := cotel.Bootstrap(context.Background(), false, "")
			require.NoError(t, err)
			t.Cleanup(func() { require.NoError(t, shutdown(context.Background())) })

			srv := InitAPIServer(cfg, dbSession, &tmocks.Client{}, &tmocks.NamespaceClient{}, sc.NewClientPool(tcfg), nil)
			// Startup work such as the JWKS fetch records its own root spans.
			// Capture only what the request below produces.
			exporter.Reset()

			rec := httptest.NewRecorder()
			req := httptest.NewRequest(http.MethodGet, fmt.Sprintf("/%s/org/test-org/%s/metadata", cfg.GetAPIRouteVersion(), cfg.GetAPIName()), nil)
			req.Header.Set("traceparent", "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01")
			srv.ServeHTTP(rec, req)
			assert.Equal(t, http.StatusUnauthorized, rec.Code)

			spans := exporter.GetSpans()
			var serverSpans tracetest.SpanStubs
			for _, span := range spans {
				if span.SpanKind == trace.SpanKindServer {
					serverSpans = append(serverSpans, span)
				}
			}
			if !tc.wantServerSpan {
				assert.Empty(t, serverSpans, "no middleware means no server span")
				return
			}
			require.Len(t, serverSpans, 1, "the middleware records one server span per request")

			wantTraceID, err := trace.TraceIDFromHex("0123456789abcdef0123456789abcdef")
			require.NoError(t, err)
			wantParentID, err := trace.SpanIDFromHex("0123456789abcdef")
			require.NoError(t, err)
			serverSpan := serverSpans[0]
			assert.Equal(t, wantTraceID, serverSpan.SpanContext.TraceID(), "the server span joins the upstream trace")
			assert.Equal(t, wantParentID, serverSpan.Parent.SpanID())
			assert.True(t, serverSpan.Parent.IsRemote())

			authSpans := 0
			for _, span := range spans {
				assert.Equal(t, wantTraceID, span.SpanContext.TraceID(),
					"spans started during the request must join the upstream trace")
				if span.Name == "AuthMiddleware" {
					authSpans++
					assert.Equal(t, serverSpan.SpanContext.SpanID(), span.Parent.SpanID(),
						"the auth span nests under the server span")
				}
			}
			assert.Equal(t, 1, authSpans)
		})
	}
}

func Test_InitTemporalClients(t *testing.T) {
	tests := []struct {
		name string
		host string
	}{
		{name: "IPv6 connection", host: "::1"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			listener, err := net.Listen("tcp", net.JoinHostPort(tt.host, "0"))
			require.NoError(t, err)

			grpcServer := grpc.NewServer()
			healthServer := health.NewServer()
			healthServer.SetServingStatus("temporal.api.workflowservice.v1.WorkflowService", healthpb.HealthCheckResponse_SERVING)
			healthpb.RegisterHealthServer(grpcServer, healthServer)
			serverDone := make(chan error, 1)
			go func() {
				serverDone <- grpcServer.Serve(listener)
			}()
			t.Cleanup(func() {
				grpcServer.Stop()
				assert.NoError(t, <-serverDone)
			})

			tcfg := &cconfig.TemporalConfig{
				Host:      tt.host,
				Port:      listener.Addr().(*net.TCPAddr).Port,
				Namespace: "cloud",
			}
			client, namespaceClient, err := InitTemporalClients(tcfg)
			require.NoError(t, err)
			t.Cleanup(client.Close)
			t.Cleanup(namespaceClient.Close)

			// Lazy construction alone does not check the connection target.
			ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
			defer cancel()
			_, err = client.CheckHealth(ctx, &temporalClient.CheckHealthRequest{})
			require.NoError(t, err)
		})
	}
}

func Test_InitMetricsServer(t *testing.T) {
	type args struct {
		e *echo.Echo
	}
	type metricsRequest struct {
		method string
		host   string
	}
	tests := []struct {
		name       string
		args       args
		requests   []metricsRequest
		wantStatus int
		wantMethod string
	}{
		{
			name: "bounds caller-controlled Host labels",
			args: args{
				e: echo.New(),
			},
			requests: []metricsRequest{
				{method: http.MethodGet, host: "nico-probe-a.invalid"},
				{method: http.MethodGet, host: "nico-probe-b.invalid"},
				{method: http.MethodGet, host: "nico-probe-a.invalid"},
			},
			wantStatus: http.StatusUnauthorized,
			wantMethod: http.MethodGet,
		},
		{
			name: "bounds non-standard HTTP method labels",
			args: args{
				e: echo.New(),
			},
			requests: []metricsRequest{
				{method: "BREW", host: "nico-probe.invalid"},
				{method: "FROB", host: "nico-probe.invalid"},
				{method: "CUSTOM-METHOD", host: "nico-probe.invalid"},
			},
			wantStatus: http.StatusUnauthorized,
			wantMethod: "unknown",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			registry := useIsolatedPrometheusRegistry(t)

			// A tracked route, since MetricsURLSkipper only records /v2/ and /metrics.
			handler := func(c echo.Context) error {
				return c.NoContent(http.StatusUnauthorized)
			}
			registeredMethods := make(map[string]bool)
			for _, metricReq := range tt.requests {
				if registeredMethods[metricReq.method] {
					continue
				}
				tt.args.e.Add(metricReq.method, "/v2/probe", handler)
				registeredMethods[metricReq.method] = true
			}
			InitMetricsServer(tt.args.e, config.DefaultMetricsNamespace)

			for _, metricReq := range tt.requests {
				req := httptest.NewRequest(metricReq.method, "/v2/probe", nil)
				req.Host = metricReq.host
				rec := httptest.NewRecorder()
				tt.args.e.ServeHTTP(rec, req)
				if tt.wantStatus != 0 {
					assert.Equal(t, tt.wantStatus, rec.Code)
				}
			}

			assertBoundedMetricsLabels(t, registry, config.DefaultMetricsNamespace, tt.wantMethod)
		})
	}
}

func TestMetricsMethodLabel(t *testing.T) {
	tests := []struct {
		name   string
		method string
		want   string
	}{
		{name: "GET", method: http.MethodGet, want: http.MethodGet},
		{name: "HEAD", method: http.MethodHead, want: http.MethodHead},
		{name: "POST", method: http.MethodPost, want: http.MethodPost},
		{name: "PUT", method: http.MethodPut, want: http.MethodPut},
		{name: "PATCH", method: http.MethodPatch, want: http.MethodPatch},
		{name: "DELETE", method: http.MethodDelete, want: http.MethodDelete},
		{name: "CONNECT", method: http.MethodConnect, want: http.MethodConnect},
		{name: "OPTIONS", method: http.MethodOptions, want: http.MethodOptions},
		{name: "TRACE", method: http.MethodTrace, want: http.MethodTrace},
		{name: "non-standard", method: "BREW", want: "unknown"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			req := httptest.NewRequest(tt.method, "/", nil)
			ctx := echo.New().NewContext(req, httptest.NewRecorder())
			assert.Equal(t, tt.want, metricsMethodLabel(ctx, nil))
		})
	}
}

func useIsolatedPrometheusRegistry(t *testing.T) *prometheus.Registry {
	t.Helper()

	registry := prometheus.NewRegistry()
	previousRegisterer := prometheus.DefaultRegisterer
	previousGatherer := prometheus.DefaultGatherer
	prometheus.DefaultRegisterer = registry
	prometheus.DefaultGatherer = registry

	t.Cleanup(func() {
		prometheus.DefaultRegisterer = previousRegisterer
		prometheus.DefaultGatherer = previousGatherer
	})

	return registry
}

func assertBoundedMetricsLabels(t *testing.T, registry *prometheus.Registry, namespace, method string) {
	t.Helper()

	families, err := registry.Gather()
	require.NoError(t, err)

	prefix := namespace + "_"
	expectedFamilies := map[string]bool{
		prefix + "requests_total":           false,
		prefix + "request_duration_seconds": false,
		prefix + "request_size_bytes":       false,
		prefix + "response_size_bytes":      false,
	}

	for _, family := range families {
		name := family.GetName()
		if _, ok := expectedFamilies[name]; !ok {
			continue
		}

		expectedFamilies[name] = true
		require.Len(t, family.GetMetric(), 1, "%s must not create a series per HTTP Host", name)

		labels := make(map[string]string)

		for _, label := range family.GetMetric()[0].GetLabel() {
			labels[label.GetName()] = label.GetValue()
		}

		assert.Equal(t, namespace, labels["host"], "%s must retain one bounded host label", name)
		assert.Equal(t, method, labels["method"], "%s must retain one bounded method label", name)
	}

	for name, found := range expectedFamilies {
		assert.True(t, found, "expected metric family %s", name)
	}
}

func testAuditSetupSchema(t *testing.T, dbSession *cdb.Session) {
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.AuditEntry)(nil))
	assert.Nil(t, err)
}

func Test_Audit(t *testing.T) {
	cfg := common.GetTestConfig()

	dbSession := cdbu.GetTestDBSession(t, true)
	defer dbSession.Close()
	// Startup syncs ConfigMap issuers into the issuer table and records history.
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Issuer)(nil), (*cdbm.StatusDetail)(nil)))
	testAuditSetupSchema(t, dbSession)

	tc := &tmocks.Client{}
	tnc := &tmocks.NamespaceClient{}

	tcfg, _ := cfg.GetTemporalConfig()

	scp := sc.NewClientPool(tcfg)

	t.Setenv("SENTRY_DSN", "https://bfe69b59461e44059a533274a6393155@glitchtip.test.com/3")

	srv := InitAPIServer(cfg, dbSession, tc, tnc, scp, nil)

	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodPost, fmt.Sprintf("/%s/org/wdksahew1rqv/%s/site", cfg.GetAPIRouteVersion(), cfg.GetAPIName()), nil)
	srv.ServeHTTP(rec, req)
	assert.Equal(t, http.StatusUnauthorized, rec.Code)
	// check if the audit log entry was created
	aeDAO := cdbm.NewAuditEntryDAO(dbSession)
	entries, count, err := aeDAO.GetAll(context.Background(), nil, cdbm.AuditEntryFilterInput{OrgName: cutil.GetPtr("wdksahew1rqv")}, paginator.PageInput{})
	assert.NoError(t, err)
	assert.Len(t, entries, 1)
	assert.Equal(t, count, 1)
	assert.Equal(t, entries[0].OrgName, "wdksahew1rqv")
	assert.Equal(t, entries[0].StatusCode, 401)
}

func Test_BodyLimit(t *testing.T) {
	cfg := common.GetTestConfig()
	dbSession := cdbu.GetTestDBSession(t, true)
	defer dbSession.Close()
	// Startup syncs ConfigMap issuers into the issuer table and records history.
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Issuer)(nil), (*cdbm.StatusDetail)(nil)))

	tc := &tmocks.Client{}
	tnc := &tmocks.NamespaceClient{}

	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	srv := InitAPIServer(cfg, dbSession, tc, tnc, scp, nil)

	oversizedBody := make([]byte, 11<<20) // 11 MiB, exceeds the 10 MiB limit
	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodPost, fmt.Sprintf("/%s/org/test-org/%s/site", cfg.GetAPIRouteVersion(), cfg.GetAPIName()), bytes.NewReader(oversizedBody))
	req.Header.Set("Content-Type", "application/json")
	srv.ServeHTTP(rec, req)
	assert.Equal(t, http.StatusRequestEntityTooLarge, rec.Code)

	normalBody := []byte(`{"name":"test"}`)
	rec2 := httptest.NewRecorder()
	req2 := httptest.NewRequest(http.MethodPost, fmt.Sprintf("/%s/org/test-org/%s/site", cfg.GetAPIRouteVersion(), cfg.GetAPIName()), bytes.NewReader(normalBody))
	req2.Header.Set("Content-Type", "application/json")
	srv.ServeHTTP(rec2, req2)
	assert.NotEqual(t, http.StatusRequestEntityTooLarge, rec2.Code)
}

func Test_NotFoundHandler(t *testing.T) {
	cfg := common.GetTestConfig()
	dbSession := cdbu.GetTestDBSession(t, true)
	defer dbSession.Close()
	// Startup syncs ConfigMap issuers into the issuer table and records history.
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Issuer)(nil), (*cdbm.StatusDetail)(nil)))

	tc := &tmocks.Client{}
	tnc := &tmocks.NamespaceClient{}

	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	srv := InitAPIServer(cfg, dbSession, tc, tnc, scp, nil)
	rec := httptest.NewRecorder()

	// Arbitrary path that should return 404
	req := httptest.NewRequest(http.MethodGet, "/test/notfound", nil)
	srv.ServeHTTP(rec, req)
	assert.Equal(t, http.StatusNotFound, rec.Code)

	// Valid route should match but return unauthorized since no auth token is provided
	rec2 := httptest.NewRecorder()
	req2 := httptest.NewRequest(http.MethodGet, fmt.Sprintf("/%s/org/test-org/%s/metadata", cfg.GetAPIRouteVersion(), cfg.GetAPIName()), nil)
	srv.ServeHTTP(rec2, req2)
	assert.Equal(t, http.StatusUnauthorized, rec2.Code)
}
