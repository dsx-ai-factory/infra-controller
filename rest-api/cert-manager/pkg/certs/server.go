// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package certs

import (
	"context"
	"crypto/tls"
	"fmt"
	"net/http"
	"sync"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/cert-manager/pkg/core"
	"github.com/getsentry/sentry-go"
	"go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp"
)

const (
	svcTTL         = 365 * 24
	svcRenewBefore = 7 * 24 * time.Hour
	svcRenewRetry  = time.Hour
)

// Options defines options for the server
type Options struct {
	Addr         string
	InsecureAddr string
	// DNSNames are the hostnames the TLS listener's certificate must match.
	// Every name clients dial has to appear here, because hostname
	// verification only consults the certificate's SANs.
	DNSNames          []string
	CABaseDNS         string
	sentryDSN         string
	AuthorizationMode string
	AllowedIdentities []string
	ClientCAFile      string
}

// Server defines a server
type Server struct {
	Options
	certificateIssuer  CertificateIssuer
	authorizer         *certificateAuthorizer
	appService         *core.HTTPService
	insecService       *core.HTTPService
	certificateMu      sync.Mutex
	servingCertificate *tls.Certificate
	nextRenewal        time.Time
}

// NewServerWithIssuer returns a server instance with the provided certificate issuer.
func NewServerWithIssuer(ctx context.Context, o Options, certIssuer CertificateIssuer) (*Server, error) {
	if certIssuer == nil {
		panic("certIssuer is required")
	}

	authorizer, err := newCertificateAuthorizer(ctx, o, certIssuer)
	if err != nil {
		return nil, err
	}
	s := &Server{Options: o, certificateIssuer: certIssuer, authorizer: authorizer}

	err = s.tlsSetup(ctx)
	if err != nil {
		return nil, err
	}

	appService := core.NewTLSService(s.Addr, "", "")
	appService.TLSConfig = &tls.Config{
		MinVersion: tls.VersionTLS12,
		ClientAuth: tls.RequestClientCert,
		GetCertificate: func(hello *tls.ClientHelloInfo) (*tls.Certificate, error) {
			return s.getCertificate(core.WithLogger(hello.Context(), core.GetLogger(ctx)))
		},
	}
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
		err = sentry.Init(sentry.ClientOptions{
			Dsn:   o.sentryDSN,
			Debug: true,
		})
		if err != nil {
			return nil, err
		}
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

// Done returns a channel closed once both HTTP services have stopped serving.
func (s *Server) Done() <-chan struct{} {
	done := make(chan struct{})

	go func() {
		defer close(done)
		<-s.appService.Done()
		<-s.insecService.Done()
	}()

	return done
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

	return s.withWraps(s.authorizeCertificateRequest(h), "ccm-get-cert")
}

func (s *Server) tlsSetup(ctx context.Context) error {
	cert, key, err := s.certificateIssuer.RawCertificate(ctx, s.DNSNames, svcTTL)
	if err != nil {
		return err
	}
	pair, err := tls.X509KeyPair([]byte(cert), []byte(key))
	if err != nil {
		return err
	}
	now := time.Now()
	if pair.Leaf == nil || !now.Before(pair.Leaf.NotAfter) || now.Before(pair.Leaf.NotBefore) {
		return fmt.Errorf("serving certificate is not currently valid")
	}
	s.servingCertificate = &pair
	s.nextRenewal = pair.Leaf.NotAfter.Add(-svcRenewBefore)
	return nil
}

// getCertificate renews on a new handshake, so ServeTLS never retains a stale
// file-loaded keypair. Renewal failures retain the valid pair and retry later.
func (s *Server) getCertificate(ctx context.Context) (*tls.Certificate, error) {
	s.certificateMu.Lock()
	defer s.certificateMu.Unlock()
	now := time.Now()
	if !now.Before(s.nextRenewal) {
		err := s.tlsSetup(ctx)
		if err != nil {
			s.nextRenewal = now.Add(svcRenewRetry)
			core.GetLogger(ctx).WithError(err).Error("failed to renew serving certificate")
			if s.servingCertificate == nil || !now.Before(s.servingCertificate.Leaf.NotAfter) {
				return nil, err
			}
		}
	}
	if s.servingCertificate == nil || !time.Now().Before(s.servingCertificate.Leaf.NotAfter) {
		return nil, fmt.Errorf("serving certificate has expired")
	}
	return s.servingCertificate, nil
}

// withWraps applies the required wrappers to the handlers
func (s *Server) withWraps(h http.Handler, oper string) http.Handler {
	oh := otelhttp.NewHandler(h, oper)
	if s.sentryDSN != "" {
		return &sentryWrap{h: oh}
	}

	return oh
}
