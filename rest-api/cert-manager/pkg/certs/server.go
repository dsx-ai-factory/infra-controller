// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package certs

import (
	"context"
	"net/http"
	"os"

	"github.com/NVIDIA/infra-controller/rest-api/cert-manager/pkg/core"
	"github.com/getsentry/sentry-go"
	"go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp"
)

const (
	svcKeyFile  = "/tmp/svc.key"
	svcCertFile = "/tmp/svc.cert"
	svcTTL      = 10 * 365 * 24
)

// Options defines options for the server
type Options struct {
	Addr         string
	InsecureAddr string
	DNSName      string
	CABaseDNS    string
	sentryDSN    string
}

// Server defines a server
type Server struct {
	Options
	certificateIssuer CertificateIssuer
	appService        *core.HTTPService
	insecService      *core.HTTPService
}

// NewServerWithIssuer returns a server instance with the provided certificate issuer.
func NewServerWithIssuer(ctx context.Context, o Options, certIssuer CertificateIssuer) (*Server, error) {
	if certIssuer == nil {
		panic("certIssuer is required")
	}

	s := &Server{Options: o}
	s.certificateIssuer = certIssuer

	err := s.tlsSetup(ctx)
	if err != nil {
		return nil, err
	}

	appService := core.NewTLSService(s.Addr, svcCertFile, svcKeyFile)
	appService.AddHealthRoute(ctx)
	appService.AddVersionRoute(ctx)
	appService.AddMetricsRoute(ctx)
	appService.Use(core.NewHTTPMiddleware(ctx, core.WithRequestMetrics("nico_rest_cert_manager"))...)
	appService.Path("/v1/pki/ca").Handler(s.PKICACertificateHandler(ctx)).Methods("GET")
	appService.Path("/v1/pki/ca/pem").Handler(s.PKICACertificateHandler(ctx)).Methods("GET")
	appService.Path("/v1/pki/cloud-cert").Handler(s.PKICloudCertificateHandler(ctx)).Methods("POST")
	s.appService = appService
	insec := core.NewHTTPService(s.InsecureAddr)
	insec.AddHealthRoute(ctx)
	insec.Path("/v1/pki/ca").Handler(s.PKICACertificateHandler(ctx)).Methods("GET")
	insec.Path("/v1/pki/ca/pem").Handler(s.PKICACertificateHandler(ctx)).Methods("GET")
	s.insecService = insec

	if o.sentryDSN != "" {
		sentry.Init(sentry.ClientOptions{
			Dsn:   o.sentryDSN,
			Debug: true,
		})
	}
	return s, nil
}

// Start starts the server
func (s *Server) Start(ctx context.Context) {
	log := core.GetLogger(ctx)

	_, err := s.appService.Start(ctx)
	if err != nil {
		log.Fatalf("failed to start appService: %v", err)
		return
	}

	_, err = s.insecService.Start(ctx)
	if err != nil {
		log.Fatalf("failed to start healt ep: %v", err)
		return
	}
}

// PKICACertificateHandler returns pkiCACertificateHandler
func (s *Server) PKICACertificateHandler(_ context.Context) http.Handler {
	h := &pkiCACertificateHandler{
		certificateIssuer: s.certificateIssuer,
	}

	return s.withWraps(h, "ccm-get-ca")
}

// PKICloudCertificateHandler returns pkiCloudCertificateHandler
func (s *Server) PKICloudCertificateHandler(_ context.Context) http.Handler {
	h := &pkiCloudCertificateHandler{
		certificateIssuer: s.certificateIssuer,
	}

	return s.withWraps(h, "ccm-get-cert")
}

func (s *Server) tlsSetup(ctx context.Context) error {
	i := s.certificateIssuer
	cert, key, err := i.RawCertificate(ctx, s.DNSName, svcTTL)
	if err != nil {
		return err
	}

	err = os.WriteFile(svcCertFile, []byte(cert), 0644)
	if err != nil {
		return err
	}

	err = os.WriteFile(svcKeyFile, []byte(key), 0644)
	if err != nil {
		return err
	}

	return nil
}

// withWraps applies the required wrappers to the handlers
func (s *Server) withWraps(h http.Handler, oper string) http.Handler {
	oh := otelhttp.NewHandler(h, oper)
	if s.sentryDSN != "" {
		return &sentryWrap{h: oh}
	}

	return oh
}
