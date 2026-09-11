// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package server

import (
	"bytes"
	"context"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"testing"

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

	tc := &tmocks.Client{}
	tnc := &tmocks.NamespaceClient{}

	tcfg, _ := cfg.GetTemporalConfig()

	scp := sc.NewClientPool(tcfg)

	t.Setenv("SENTRY_DSN", "https://bfe69b59461e44059a533274a6393155@glitchtip.test.com/3")

	tests := []struct {
		name string
		args args
		want *echo.Echo
	}{
		{
			name: "test initAPIServer success",
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
			InitAPIServer(tt.args.cfg, tt.args.dbSession, tt.args.tc, tt.args.tnc, tt.args.scp, nil)
		})
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
			rec := httptest.NewRecorder()
			req := httptest.NewRequest(http.MethodGet, fmt.Sprintf("/%s/org/test-org/%s/metadata", cfg.GetAPIRouteVersion(), cfg.GetAPIName()), nil)
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
			for _, span := range spans {
				assert.Equal(t, serverSpans[0].SpanContext.TraceID(), span.SpanContext.TraceID(),
					"spans started during the request must join the server span's trace")
			}
		})
	}
}

func Test_InitTemporalClients(t *testing.T) {
	keyPath, certPath := config.SetupTestCerts(t)
	defer os.Remove(keyPath)
	defer os.Remove(certPath)

	cfg := common.GetTestConfig()
	cfg.SetTemporalCertPath(certPath)
	cfg.SetTemporalKeyPath(keyPath)
	cfg.SetTemporalCaPath(certPath)

	tcfg, err := cfg.GetTemporalConfig()
	assert.NoError(t, err)
	defer cfg.Close()

	type args struct {
		tConfig *cconfig.TemporalConfig
	}

	tests := []struct {
		name string
		args args
	}{
		{
			name: "test initTemporalClient success",
			args: args{
				tConfig: tcfg,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			InitTemporalClients(tt.args.tConfig)
		})
	}
}

func Test_InitMetricsServer(t *testing.T) {
	type args struct {
		e *echo.Echo
	}
	tests := []struct {
		name string
		args args
	}{
		{
			name: "test initMetricsServer success",
			args: args{
				e: echo.New(),
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// A tracked route, since MetricsURLSkipper only records /v2/ and /metrics.
			tt.args.e.GET("/v2/probe", func(c echo.Context) error {
				return c.NoContent(http.StatusOK)
			})

			InitMetricsServer(tt.args.e, config.DefaultMetricsNamespace)

			tt.args.e.ServeHTTP(httptest.NewRecorder(), httptest.NewRequest(http.MethodGet, "/v2/probe", nil))

			// The prefix is the published contract. An empty Subsystem would
			// silently produce echo_requests_total instead.
			families, err := prometheus.DefaultGatherer.Gather()
			assert.NoError(t, err)

			names := make([]string, 0, len(families))
			for _, family := range families {
				names = append(names, family.GetName())
			}
			assert.Contains(t, names, "nico_rest_api_requests_total")
			assert.Contains(t, names, "nico_rest_api_request_duration_seconds")
		})
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
