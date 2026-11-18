// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sites

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"os"
	"time"

	"github.com/labstack/echo/v4"
	"github.com/pkg/errors"
	"github.com/rs/zerolog/log"
	"go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp"
	"k8s.io/client-go/rest"

	"github.com/NVIDIA/infra-controller/rest-api/site-manager/internal/certmanager"
	"github.com/NVIDIA/infra-controller/rest-api/site-manager/internal/server"
	"github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/api"
	crdclient "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/client/clientset/versioned"
	"github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/metadata"
)

const (
	oneYrHours    = 365 * 24
	mgrCertFile   = "/tmp/mgr.cert"
	mgrKeyFile    = "/tmp/mgr.key"
	crdClientName = "site-crd-client"
)

// Options are args passed to the site manager at boot
type Options struct {
	credsMgrURL string
	// credsMgrCAPath is the CA bundle used to verify credsMgrURL. It is the
	// ca.crt of the same CA that signs the cert-manager's listener, so the
	// certificate chain and the hostname both have to check out.
	credsMgrCAPath string
	ingressHost    string
	listenPort     string
	tlsKeyPath     string
	tlsCertPath    string
	namespace      string
	// otpDuration is how long a Site's OTP stays valid.
	otpDuration time.Duration
}

// SiteManager defines an instance of Site Manager
type SiteManager struct {
	Options
	echo        *echo.Echo
	certManager *certmanager.Client
	l           net.Listener
}

// NewSiteManager creates an instance of SiteManager that stores Sites
// through crdClient.
func NewSiteManager(ctx context.Context, o Options, crdClient crdclient.Interface) (*SiteManager, error) {
	certManager, err := certmanager.NewClient(o.credsMgrURL, o.credsMgrCAPath)
	if err != nil {
		return nil, errors.Wrap(err, "certmanager.NewClient")
	}

	s := &SiteManager{
		Options:     o,
		certManager: certManager,
	}

	err = s.tlsSetup(ctx)
	if err != nil {
		return nil, errors.Wrap(err, "tlsSetup")
	}
	sites := crdClient.ForgeV1().Sites(s.namespace)
	s.echo = server.InitSiteManagerServer(
		"nico_rest_site_manager",
		api.NewSystemRoutes(metadata.Version),
		api.NewSiteRoutes(sites, s.otpDuration, certManager),
	)
	return s, nil
}

// newCRDClient returns a Site CRD client for the cluster Site Manager runs in.
// Its requests are traced.
func newCRDClient() (crdclient.Interface, error) {
	cfg, err := rest.InClusterConfig()
	if err != nil {
		return nil, errors.Wrap(err, "rest.InClusterConfig()")
	}

	// wrap the transport with otel
	cfg.WrapTransport = func(rt http.RoundTripper) http.RoundTripper {
		return otelhttp.NewTransport(rt, otelhttp.WithSpanNameFormatter(func(string, *http.Request) string {
			return crdClientName
		}))
	}

	c, err := crdclient.NewForConfig(cfg)
	if err != nil {
		return nil, errors.Wrap(err, "crdclient.NewForConfig(cfg)")
	}
	return c, nil
}

// Start starts the manager
func (s *SiteManager) Start(ctx context.Context) {
	l, err := server.ServeTLS(ctx, s.echo, fmt.Sprintf(":%s", s.listenPort), s.tlsCertPath, s.tlsKeyPath)
	if err != nil {
		log.Fatal().Err(err).Msg("failed to start the server")
	}
	s.l = l
}

func (s *SiteManager) tlsSetup(ctx context.Context) error {
	if s.tlsCertPath != "" && s.tlsKeyPath != "" {
		log.Info().Str("cert_path", s.tlsCertPath).Str("key_path", s.tlsKeyPath).Msg("using the TLS certificate and key passed on the command line")
		return nil
	}

	log.Info().Msg("getting a TLS certificate from Cert Manager")

	var (
		resp *certmanager.Certificate
		err  error
	)

	for {
		resp, err = s.certManager.IssueCertificate(ctx, s.ingressHost, "", oneYrHours)
		if err == nil {
			break
		}
		log.Info().Err(err).Msg("failed to get a TLS certificate from Cert Manager, retrying in 10s")
		// A shutdown signal cancels ctx, which ends the retries.
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(10 * time.Second):
		}
	}
	err = os.WriteFile(mgrCertFile, []byte(resp.Certificate), 0644)
	if err != nil {
		return err
	}

	err = os.WriteFile(mgrKeyFile, []byte(resp.Key), 0644)
	if err != nil {
		return err
	}

	s.tlsCertPath = mgrCertFile
	s.tlsKeyPath = mgrKeyFile
	return nil
}
