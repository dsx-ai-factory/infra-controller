// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package core

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"os"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/felixge/httpsnoop"
	"github.com/gorilla/mux"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
	"github.com/prometheus/client_golang/prometheus/promhttp"
	"github.com/sirupsen/logrus"

	"go.opentelemetry.io/contrib/instrumentation/github.com/gorilla/mux/otelmux"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/codes"
	semconv "go.opentelemetry.io/otel/semconv/v1.11.0"
	"go.opentelemetry.io/otel/trace"
)

// HTTPService augments mux.Router with:
//
// - Reasonable defaults
//
//   - Helper methods for adding common routes (/healthz, /version,
//     /metrics, /admin ...).
//
//   - Handle both tcp addresses (e.g., :8000) and unix socket addresses
//     (e.g., unix:///tmp/test.sock)
//
//   - A non-blocking Start(ctx) method handles grace shutdown when
//     receiving ctx.Done().
type HTTPService struct {
	*mux.Router

	// Addr could be a TCP address like:
	//
	// - 0.0.0.0:8000 (any interface, port 8000)
	// - 127.0.0.1:8000 (loop-back interface only, port 8000)
	// - 0.0.0.0:0 (any interface, any free port)
	//
	// Addr could also be prefix with `unix://` (e.g.,
	// `unix:///tmp/test.sock`), which will make HTTPService listen to a
	// local unix domain socket instead of a network interface.
	Addr string

	// SocketPermission is only applicable when addr is a unix socket
	// path, e.g., 'unix:///tmp/test.sock'
	SocketPermission os.FileMode

	// ReadTimeout, WriteTimeout and IdleTimeout are timeout
	// configurations passed to go http.Server.
	ReadTimeout, WriteTimeout, IdleTimeout time.Duration

	// ShutDownGracePeriod is the grace period for
	// `http.Server:Shutdown`
	ShutDownGracePeriod time.Duration

	// CertFile, KeyFile are used if TLS is required.
	// They point to the corresponding files.
	CertFile, KeyFile string

	// for internal use
	isTLS bool
}

// NewHTTPService creates a new service
func NewHTTPService(addr string) *HTTPService {
	return &HTTPService{
		Router:              mux.NewRouter(),
		Addr:                addr,
		SocketPermission:    os.FileMode(0666),
		ReadTimeout:         5 * time.Second,
		WriteTimeout:        10 * time.Second,
		IdleTimeout:         120 * time.Second,
		ShutDownGracePeriod: 3 * time.Second,
	}
}

// NewTLSService creates a new TLS service
func NewTLSService(addr, certFile, keyFile string) *HTTPService {
	return &HTTPService{
		Router:              mux.NewRouter(),
		Addr:                addr,
		SocketPermission:    os.FileMode(0666),
		ReadTimeout:         5 * time.Second,
		WriteTimeout:        10 * time.Second,
		IdleTimeout:         120 * time.Second,
		ShutDownGracePeriod: 3 * time.Second,
		KeyFile:             keyFile,
		CertFile:            certFile,
		isTLS:               true,
	}
}

// AddHealthRoute adds a health route
func (s *HTTPService) AddHealthRoute(ctx context.Context) { HTTPAddHealthRoute(ctx, s.Router) }

// AddVersionRoute adds a version route
func (s *HTTPService) AddVersionRoute(ctx context.Context) { HTTPAddVersionRoute(ctx, s.Router) }

// AddMetricsRoute adds a metrics route
func (s *HTTPService) AddMetricsRoute(ctx context.Context) { HTTPAddMetricsRoute(ctx, s.Router) }

// AddAdminRoute adds an admin route
func (s *HTTPService) AddAdminRoute(ctx context.Context) { HTTPAddAdminRoute(ctx, s.Router) }

func (s *HTTPService) initListener(ctx context.Context) (net.Listener, error) {
	log := GetLogger(ctx)

	if strings.HasPrefix(s.Addr, "unix://") {
		log.Infof("Initializing unix domain socket listener at %q ...", s.Addr)
		socket := strings.TrimPrefix(s.Addr, "unix://")
		if err := os.Remove(socket); err != nil {
			log.Errorf("os.Remove(%s), err: %s", socket, err.Error())
		}

		listener, err := net.Listen("unix", socket)
		if err != nil {
			return nil, err
		}

		if err := os.Chmod(socket, s.SocketPermission); err != nil {
			log.Errorf("failed to os.Chmod(%s, %v), err: %s", socket, s.SocketPermission, err.Error())
			return nil, err
		}

		return listener, nil
	}

	log.Infof("Initializing tcp listener at %q ...", s.Addr)
	return net.Listen("tcp", s.Addr)
}

// Start starts the service
func (s *HTTPService) Start(ctx context.Context) (net.Listener, error) {
	log := GetLogger(ctx)

	server := &http.Server{
		Handler:      s.Router,
		ReadTimeout:  s.ReadTimeout,
		WriteTimeout: s.WriteTimeout,
		IdleTimeout:  s.IdleTimeout,
	}
	listener, err := s.initListener(ctx)
	if err != nil {
		return nil, err
	}

	if s.isTLS {
		go func() {
			log.Infof("Serving HTTPS at: %v", listener.Addr())
			if err := server.ServeTLS(listener, s.CertFile, s.KeyFile); err != nil {
				log.Error(err)
			}
		}()
	} else {
		go func() {
			log.Infof("Serving HTTP at: %v", listener.Addr())
			if err := server.Serve(listener); err != nil {
				log.Error(err)
			}
		}()
	}

	go func() {
		<-ctx.Done()

		newCtx, cancel := context.WithTimeout(context.Background(), s.ShutDownGracePeriod)
		defer cancel()

		log.Infof("Shutting down HTTPService at %v", listener.Addr())
		err = server.Shutdown(newCtx)
		if err != nil {
			log.Infof("failed to terminate HTTPService at %v, err: %v", listener.Addr(), err)
			return
		}
		log.Infof("Terminated HTTPService at %v", listener.Addr())
	}()

	return listener, nil
}

const httpHealthRoutePath = "/healthz"

// HTTPAddHealthRoute adds a health route
func HTTPAddHealthRoute(ctx context.Context, r *mux.Router) {
	r.Path(httpHealthRoutePath).Handler(HTTPHealthHandler(ctx)).Methods("GET")
}

// HTTPHealthHandler handles the health ep
func HTTPHealthHandler(_ context.Context) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		log := GetLogger(r.Context())
		w.WriteHeader(http.StatusOK)
		_, err := w.Write([]byte("ok\n"))
		if err != nil {
			log.Error(err)
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
	})
}

// HTTPAddVersionRoute adds a version route
func HTTPAddVersionRoute(ctx context.Context, r *mux.Router) {
	r.Path("/version").Handler(HTTPVersionHandler(ctx)).Methods("GET")
}

// HTTPVersionHandler handles the version ep
func HTTPVersionHandler(_ context.Context) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		log := GetLogger(r.Context())
		w.WriteHeader(http.StatusOK)
		_, err := w.Write([]byte("0.1.0" + "\n"))
		if err != nil {
			log.Error(err)
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
	})
}

// HTTPAddMetricsRoute adds a metric route
func HTTPAddMetricsRoute(_ context.Context, r *mux.Router) {
	r.Path("/metrics").Handler(promhttp.Handler()).Methods("GET")
}

// HTTPAddAdminRoute adds the admin route
func HTTPAddAdminRoute(ctx context.Context, r *mux.Router) {
	r.Path("/admin").Handler(HTTPAdminHandler(ctx)).Methods("GET")
}

// HTTPAdminHandler provides debugging utilities. It should usually
// listen to 127.0.0.1 instead of exposing publicly (unless authNed
// and authZed). Currently admin handler allows update logging level
// while application is running, e.g.:
//
// curl http://127.0.0.1:8002/admin?log-level=debug
func HTTPAdminHandler(ctx context.Context) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		log := GetLogger(r.Context())

		replyCurrentConfig := func(w http.ResponseWriter, _ *http.Request) {
			type adminConfig struct {
				LogLevel logrus.Level `json:"log-level"`
			}
			resp := &adminConfig{}
			resp.LogLevel = GetLogger(ctx).Logger.GetLevel()

			data, err := json.MarshalIndent(resp, "", "  ")
			if err != nil {
				http.Error(w, err.Error(), http.StatusInternalServerError)
				return
			}
			w.WriteHeader(http.StatusOK)
			_, errWrite := w.Write(append(data, []byte("\n")...))
			if errWrite != nil {
				log.Error(errWrite)
				http.Error(w, errWrite.Error(), http.StatusInternalServerError)
				return
			}
		}

		// No configuration set, return current configuration
		if len(r.URL.Query()) == 0 {
			replyCurrentConfig(w, r)
			return
		}

		// Apply new configurations
		if l := r.URL.Query().Get("log-level"); l != "" {
			level, err := logrus.ParseLevel(l)
			if err != nil {
				log.Error(err)
				http.Error(w, err.Error(), http.StatusBadRequest)
				return
			}

			log.Infof("Setting global logging level to %s", level)
			GetLogger(ctx).Logger.SetLevel(level)
			log.Infof("Successfully set global logging level to %s", level)
		}

		replyCurrentConfig(w, r)
	})
}

type httpMiddleware struct {
	latencyMetricsName      string
	handlerTimeout          time.Duration
	requestBodyLimit        int64
	telemetryServiceName    string
	telemetryExcludedRoutes map[string]interface{}
}

// HTTPMiddlewareOption defines a middleware option
type HTTPMiddlewareOption func(*httpMiddleware)

// MetricsNamespaceEnv overrides the prefix a caller passes to WithRequestMetrics.
// These services read no config file, so an environment variable is the only
// override available to an operator.
const MetricsNamespaceEnv = "METRICS_NAMESPACE"

// WithRequestMetrics enables service specific metrics regarding
// request duration and count. By default these metrics are disabled.
// serverName is the prefix applied to the metric names, and METRICS_NAMESPACE
// takes precedence over it when set.
func WithRequestMetrics(serverName string) HTTPMiddlewareOption {
	return func(h *httpMiddleware) {
		h.latencyMetricsName = serverName
		if namespace := os.Getenv(MetricsNamespaceEnv); namespace != "" {
			h.latencyMetricsName = namespace
		}
	}
}

// WithHandlerTimeout allows one to modify the handler timeout.
// Default is 5 seconds.
func WithHandlerTimeout(t time.Duration) HTTPMiddlewareOption {
	return func(h *httpMiddleware) {
		h.handlerTimeout = t
	}
}

// WithRequestBodyLimit modifies the request body limit.
// Default is 1MB.
func WithRequestBodyLimit(limit int64) HTTPMiddlewareOption {
	return func(h *httpMiddleware) {
		h.requestBodyLimit = limit
	}
}

// WithTelemetry enables telemetry sets the http
func WithTelemetry(serverName string) HTTPMiddlewareOption {
	return func(h *httpMiddleware) {
		h.telemetryServiceName = serverName
	}
}

// NewHTTPMiddleware builds a http middleware that:
//
//   - Adds contexted request logger with {method,path,ip} into
//     request.Context
//
//   - Collects <latencyMetricsName>_http_duration_seconds metrics with
//     {path,method} dimensions, if latencyMetricsName is specified
//
// - Wraps handler with http.TimeoutHandler with `handlerTimeout`.
//
// - Wraps request.Body with MaxBytesReader with `requestBodyLimit`.
//
//   - Instruments OpenTelemetry by adding a span to the request context
//     that tracks the response and its return codes
func NewHTTPMiddleware(ctx context.Context, opts ...HTTPMiddlewareOption) []mux.MiddlewareFunc {
	m := &httpMiddleware{
		handlerTimeout:   5 * time.Second,
		requestBodyLimit: int64(1 << 20), // 1 MB
		telemetryExcludedRoutes: map[string]interface{}{
			// Exclude /healthz
			httpHealthRoutePath: nil,
		},
	}

	// Apply options
	for _, o := range opts {
		o(m)
	}

	var mw []mux.MiddlewareFunc

	// Add telemetry middleware if enabled
	if m.telemetryServiceName != "" {
		mw = append(mw, telemetryMiddleware(m.telemetryServiceName, m.telemetryExcludedRoutes))
	}

	mw = append(mw, loggingMiddleware(ctx), metricsMiddleware(m.latencyMetricsName))

	// Add timeout middleware
	if m.handlerTimeout > 0 {
		mw = append(mw, timeoutMiddleware(m.handlerTimeout))
	}

	// Add max request body middleware
	if m.requestBodyLimit > 0 {
		mw = append(mw, maxRequestBodyMiddleware(m.requestBodyLimit))
	}

	return mw
}

var httpReqSequenceID = uint64(0)

// nextRequestID returns a 6 digits string as request ID
func nextRequestID(_ context.Context, sofar *uint64) string {
	next := atomic.AddUint64(sofar, 1)
	return fmt.Sprintf("%06d", next%1000000)
}

// recordingResponseWriter records the status of the http request
type recordingResponseWriter struct {
	writer     http.ResponseWriter
	written    bool
	statusCode int
}

var rrwPool = &sync.Pool{
	New: func() interface{} {
		return &recordingResponseWriter{}
	},
}

func getRRW(writer http.ResponseWriter) *recordingResponseWriter {
	rrw := rrwPool.Get().(*recordingResponseWriter)
	rrw.written = false
	rrw.statusCode = 0
	rrw.writer = httpsnoop.Wrap(writer, httpsnoop.Hooks{
		Write: func(next httpsnoop.WriteFunc) httpsnoop.WriteFunc {
			return func(b []byte) (int, error) {
				if !rrw.written {
					rrw.written = true
					rrw.statusCode = http.StatusOK
				}
				return next(b)
			}
		},
		WriteHeader: func(next httpsnoop.WriteHeaderFunc) httpsnoop.WriteHeaderFunc {
			return func(statusCode int) {
				if !rrw.written {
					rrw.written = true
					rrw.statusCode = statusCode
				}
				next(statusCode)
			}
		},
	})
	return rrw
}

func putRRW(rrw *recordingResponseWriter) {
	rrw.writer = nil
	rrwPool.Put(rrw)
}

func telemetryMiddleware(serviceName string, excludedRoutes map[string]interface{}) mux.MiddlewareFunc {
	otelMW := otelmux.Middleware(serviceName)
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(rw http.ResponseWriter, r *http.Request) {
			route := mux.CurrentRoute(r)
			var spanName string
			if route != nil {
				var err error
				spanName, err = route.GetPathTemplate()
				if err != nil {
					spanName, err = route.GetPathRegexp()
					if err != nil {
						spanName = ""
					}
				}
			}

			// Check if the route is excluded if so skip the otel middleware
			if _, ok := excludedRoutes[spanName]; ok {
				next.ServeHTTP(rw, r)
			} else {
				// route is not excluded call the httpmiddleware
				otelMW(next).ServeHTTP(rw, r)
			}
		})
	}
}

func loggingMiddleware(rootCtx context.Context) mux.MiddlewareFunc {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(rw http.ResponseWriter, r *http.Request) {
			reqID := nextRequestID(rootCtx, &httpReqSequenceID)
			logFields := map[string]interface{}{
				"path":        r.URL.Path,
				"method":      r.Method,
				"ip":          r.RemoteAddr,
				"http_req_id": reqID,
			}
			// Retrieve the span and trace IDs if they exist and log them
			spanCtx := trace.SpanContextFromContext(r.Context())
			if spanCtx.HasSpanID() {
				logFields["span_id"] = spanCtx.SpanID().String()
			}
			if spanCtx.HasTraceID() {
				logFields["trace_id"] = spanCtx.TraceID().String()
			}

			// Create new logger with fields and pass along
			requestLogger := GetLogger(rootCtx).WithFields(logFields)
			next.ServeHTTP(rw, r.WithContext(WithLogger(r.Context(), requestLogger)))
		})
	}
}

func metricsMiddleware(latencyMetricsName string) mux.MiddlewareFunc {
	var latency *prometheus.HistogramVec
	var count *prometheus.CounterVec
	if latencyMetricsName != "" {
		latency = promauto.NewHistogramVec(prometheus.HistogramOpts{
			Name: fmt.Sprintf("%s_http_duration_seconds", latencyMetricsName),
			Help: "Duration of HTTP requests.",
		}, []string{"path", "method"})
		count = promauto.NewCounterVec(prometheus.CounterOpts{
			Name: fmt.Sprintf("%s_http_request_counts", latencyMetricsName),
			Help: "Counter of HTTP requests.",
		}, []string{"path", "method", "status_code"})
	}

	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(rw http.ResponseWriter, r *http.Request) {
			log := GetLogger(r.Context())
			w := getRRW(rw)
			defer func() {
				log.WithField("status_code", w.statusCode).Debugf("Handler finished")
				if count != nil {
					count.WithLabelValues(r.URL.Path, r.Method, fmt.Sprintf("%d", w.statusCode)).Inc()
				}
				// TODO(mcamp) this is a hack to get lightstep to recognize the span
				// as an error. Either lightstep launcher or otelmux (probably the latter) should give us
				// a hook to set this attribute. Until then we'll just set it here.
				if v, _ := semconv.SpanStatusFromHTTPStatusCode(w.statusCode); v == codes.Error {
					trace.SpanFromContext(r.Context()).SetAttributes(attribute.Bool("error", true))
				}
				// Return the writer back to the pool
				putRRW(w)
			}()
			log.Debugf("Handler started")

			if latency != nil {
				timer := prometheus.NewTimer(latency.WithLabelValues(r.URL.Path, r.Method))
				next.ServeHTTP(w.writer, r)
				timer.ObserveDuration()
			} else {
				next.ServeHTTP(w.writer, r)
			}
		})
	}
}

func timeoutMiddleware(handlerTimeout time.Duration) mux.MiddlewareFunc {
	return func(next http.Handler) http.Handler {
		timeoutMsg := fmt.Sprintf("Request timed out in %v\n", handlerTimeout)
		return http.TimeoutHandler(next, handlerTimeout, timeoutMsg)
	}
}

func maxRequestBodyMiddleware(requestBodyLimit int64) mux.MiddlewareFunc {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			r.Body = http.MaxBytesReader(w, r.Body, requestBodyLimit)
			next.ServeHTTP(w, r)
		})
	}
}
