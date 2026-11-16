// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package certs

import (
	"context"
	"crypto/x509"
	"fmt"
	"net/http"
	"os"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/cert-manager/pkg/core"
	"github.com/spiffe/go-spiffe/v2/spiffeid"
	"github.com/spiffe/go-spiffe/v2/svid/x509svid"
)

const (
	authModeAudit   = "AUDIT"
	authModeEnforce = "ENFORCE"
)

// certificateAuthorizer verifies the client chain and exact SPIFFE URI at the
// issuance route, leaving CA retrieval and audit-mode handshakes public.
type certificateAuthorizer struct {
	mode    string
	allowed map[string]struct{}
	roots   *x509.CertPool
}

func newCertificateAuthorizer(ctx context.Context, o Options, issuer CertificateIssuer) (*certificateAuthorizer, error) {
	mode := strings.TrimSpace(o.AuthorizationMode)
	if mode == "" {
		mode = authModeAudit
	}
	if mode != authModeAudit && mode != authModeEnforce {
		return nil, fmt.Errorf("authorization mode must be AUDIT or ENFORCE, got %q", mode)
	}
	if mode == authModeEnforce && len(o.AllowedIdentities) == 0 {
		return nil, fmt.Errorf("ENFORCE requires at least one allowed SPIFFE identity")
	}
	allowed := make(map[string]struct{}, len(o.AllowedIdentities))
	for _, value := range o.AllowedIdentities {
		id, err := spiffeid.FromString(value)
		if err != nil || id.Path() == "" || id.String() != value {
			return nil, fmt.Errorf("invalid SPIFFE workload identity %q", value)
		}
		_, exists := allowed[value]
		if exists {
			return nil, fmt.Errorf("duplicate SPIFFE identity %q", value)
		}
		allowed[value] = struct{}{}
	}
	var bundle []byte
	if o.ClientCAFile != "" {
		var err error
		bundle, err = os.ReadFile(o.ClientCAFile)
		if err != nil {
			return nil, fmt.Errorf("read client CA: %w", err)
		}
	} else {
		pem, err := issuer.GetCACertificate(ctx)
		if err != nil {
			return nil, fmt.Errorf("load signing CA for client verification: %w", err)
		}
		bundle = []byte(pem)
	}
	roots := x509.NewCertPool()
	if !roots.AppendCertsFromPEM(bundle) {
		return nil, fmt.Errorf("client CA bundle contains no certificates")
	}
	return &certificateAuthorizer{mode: mode, allowed: allowed, roots: roots}, nil
}

func (a *certificateAuthorizer) identity(r *http.Request) (string, error) {
	if r.TLS == nil || len(r.TLS.PeerCertificates) == 0 {
		return "", fmt.Errorf("client certificate is missing")
	}
	intermediates := x509.NewCertPool()
	for _, cert := range r.TLS.PeerCertificates[1:] {
		intermediates.AddCert(cert)
	}
	leaf := r.TLS.PeerCertificates[0]
	_, err := leaf.Verify(x509.VerifyOptions{Roots: a.roots, Intermediates: intermediates, KeyUsages: []x509.ExtKeyUsage{x509.ExtKeyUsageClientAuth}})
	if err != nil {
		return "", fmt.Errorf("verify client certificate: %w", err)
	}
	id, err := x509svid.IDFromCert(leaf)
	if err != nil || id.Path() == "" {
		return "", fmt.Errorf("client certificate requires one SPIFFE workload URI")
	}
	return id.String(), nil
}

func (s *Server) authorizeCertificateRequest(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		a := s.authorizer
		identity, err := a.identity(r)
		status := http.StatusUnauthorized
		reason := ""
		_, allowed := a.allowed[identity]
		if err != nil {
			reason = err.Error()
		} else if !allowed {
			status = http.StatusForbidden
			reason = "SPIFFE identity is not allowed"
		}
		if reason != "" {
			core.GetLogger(r.Context()).WithField("authorization_mode", a.mode).
				WithField("service_identity", identity).WithField("rejection_reason", reason).
				Warn("certificate issuance authorization rejected caller")
			if a.mode == authModeEnforce {
				http.Error(w, http.StatusText(status), status)
				return
			}
		} else {
			core.GetLogger(r.Context()).WithField("authorization_mode", a.mode).
				WithField("service_identity", identity).Info("certificate issuance authorized caller")
		}
		next.ServeHTTP(w, r)
	})
}
