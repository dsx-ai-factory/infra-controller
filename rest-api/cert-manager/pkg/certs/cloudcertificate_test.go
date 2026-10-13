// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package certs

import (
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/pem"
	"io"
	"math/big"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/cert-manager/pkg/core"
	"github.com/NVIDIA/infra-controller/rest-api/cert-manager/pkg/pki"
	"github.com/NVIDIA/infra-controller/rest-api/cert-manager/pkg/types"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

const siteManagerIdentity = "spiffe://nico.test/nico-rest/sa/nico-rest-site-manager"

type recordingCertificateIssuer struct {
	CertificateIssuer
	requests []*CertificateRequest
	err      error
}

func (i *recordingCertificateIssuer) NewCertificate(_ context.Context, req *CertificateRequest) (string, string, error) {
	i.requests = append(i.requests, req)
	return "test-certificate", "test-private-key", i.err
}

type testAuthority struct {
	cert *x509.Certificate
	key  *ecdsa.PrivateKey
	pem  []byte
}

func newTestAuthority(t *testing.T) testAuthority {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	require.NoError(t, err)
	template := &x509.Certificate{SerialNumber: big.NewInt(1), Subject: pkix.Name{CommonName: "Test CA"}, IsCA: true, BasicConstraintsValid: true, KeyUsage: x509.KeyUsageCertSign, NotBefore: time.Now().Add(-time.Hour), NotAfter: time.Now().Add(time.Hour)}
	der, err := x509.CreateCertificate(rand.Reader, template, template, &key.PublicKey, key)
	require.NoError(t, err)
	cert, err := x509.ParseCertificate(der)
	require.NoError(t, err)
	return testAuthority{cert: cert, key: key, pem: pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der})}
}
func (ca testAuthority) clientCertificate(t *testing.T, uris []string, expired, serverOnly bool) tls.Certificate {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	require.NoError(t, err)
	template := &x509.Certificate{SerialNumber: big.NewInt(2), NotBefore: time.Now().Add(-time.Hour), NotAfter: time.Now().Add(time.Hour), KeyUsage: x509.KeyUsageDigitalSignature, ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageClientAuth}, DNSNames: []string{"nico-rest-site-manager.nico-rest.svc.cluster.local"}}
	if expired {
		template.NotAfter = time.Now().Add(-time.Minute)
	}
	if serverOnly {
		template.ExtKeyUsage = []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth}
	}
	for _, value := range uris {
		uri, parseErr := url.Parse(value)
		require.NoError(t, parseErr)
		template.URIs = append(template.URIs, uri)
	}
	der, err := x509.CreateCertificate(rand.Reader, template, ca.cert, &key.PublicKey, ca.key)
	require.NoError(t, err)
	return tls.Certificate{Certificate: [][]byte{der}, PrivateKey: key}
}
func (ca testAuthority) issuer(t *testing.T) CertificateIssuer {
	t.Helper()
	dir := t.TempDir()
	certPath, keyPath := filepath.Join(dir, "ca.crt"), filepath.Join(dir, "ca.key")
	keyDER, err := x509.MarshalECPrivateKey(ca.key)
	require.NoError(t, err)
	require.NoError(t, os.WriteFile(certPath, ca.pem, 0600))
	require.NoError(t, os.WriteFile(keyPath, pem.EncodeToMemory(&pem.Block{Type: "EC PRIVATE KEY", Bytes: keyDER}), 0600))
	issuer, err := pki.NewNativeCertificateIssuer(pki.NativeCertificateIssuerOptions{CACertFile: certPath, CAKeyFile: keyPath, BaseDNS: "nico.test"})
	require.NoError(t, err)
	return issuer
}

func TestServer_PKICloudCertificateHandler(t *testing.T) {
	ca := newTestAuthority(t)
	otherCA := newTestAuthority(t)
	issuer := &recordingCertificateIssuer{CertificateIssuer: ca.issuer(t)}
	logCtx, hook := core.WithTestingLogger(context.Background())
	ctx, cancel := context.WithCancel(logCtx)
	defer cancel()
	server, err := NewServerWithIssuer(ctx, Options{Addr: "127.0.0.1:0", DNSNames: []string{"localhost"}, AuthorizationMode: authModeEnforce, AllowedIdentities: []string{siteManagerIdentity}}, issuer)
	require.NoError(t, err)
	listener, err := server.appService.Start(ctx)
	require.NoError(t, err)
	t.Cleanup(func() { cancel(); <-server.appService.Done() })
	roots := x509.NewCertPool()
	require.True(t, roots.AppendCertsFromPEM(ca.pem))
	for _, tc := range []struct {
		name                                 string
		mode                                 string
		uris                                 []string
		noCert, wrongCA, expired, serverOnly bool
		path                                 string
		issuerErr                            error
		status                               int
	}{
		{name: "missing certificate", mode: authModeEnforce, noCert: true, status: 401},
		{name: "untrusted signer", mode: authModeEnforce, wrongCA: true, uris: []string{siteManagerIdentity}, status: 401},
		{name: "wrong identity", mode: authModeEnforce, uris: []string{"spiffe://nico.test/other/sa/nico-rest-site-manager"}, status: 403},
		{name: "legacy DNS certificate cannot impersonate URI", mode: authModeEnforce, status: 401},
		{name: "expired certificate", mode: authModeEnforce, expired: true, uris: []string{siteManagerIdentity}, status: 401},
		{name: "wrong extended key usage", mode: authModeEnforce, serverOnly: true, uris: []string{siteManagerIdentity}, status: 401},
		{name: "ambiguous URI", mode: authModeEnforce, uris: []string{siteManagerIdentity, "spiffe://nico.test/another"}, status: 401},
		{name: "authorized caller", mode: authModeEnforce, uris: []string{siteManagerIdentity}, status: 200},
		{name: "invalid lifetime maps to bad request", mode: authModeEnforce, uris: []string{siteManagerIdentity}, issuerErr: types.ErrInvalidTTL, status: 400},
		{name: "audit records authorized identity", mode: authModeAudit, uris: []string{siteManagerIdentity}, status: http.StatusOK},
		{name: "audit permits legacy caller", mode: authModeAudit, noCert: true, status: 200},
		{name: "audit permits expired certificate past handshake", mode: authModeAudit, expired: true, uris: []string{siteManagerIdentity}, status: 200},
		{name: "audit permits wrong identity", mode: authModeAudit, uris: []string{"spiffe://nico.test/other"}, status: 200},
		{name: "CA retrieval remains public", mode: authModeEnforce, noCert: true, path: "/v1/pki/ca/pem", status: 200},
	} {
		t.Run(tc.name, func(t *testing.T) {
			hook.Reset()
			server.authorizer.mode = tc.mode
			issuer.requests = nil
			issuer.err = tc.issuerErr
			config := &tls.Config{MinVersion: tls.VersionTLS12, RootCAs: roots, ServerName: "localhost"}
			if !tc.noCert {
				signer := ca
				if tc.wrongCA {
					signer = otherCA
				}
				certificate := signer.clientCertificate(t, tc.uris, tc.expired, tc.serverOnly)
				config.GetClientCertificate = func(*tls.CertificateRequestInfo) (*tls.Certificate, error) { return &certificate, nil }
			}
			transport := &http.Transport{TLSClientConfig: config}
			defer transport.CloseIdleConnections()
			client := &http.Client{Transport: transport, Timeout: 5 * time.Second}
			path, method := tc.path, http.MethodGet
			if path == "" {
				path = "/v1/pki/cloud-cert"
				method = http.MethodPost
			}
			req, err := http.NewRequestWithContext(ctx, method, "https://"+listener.Addr().String()+path, strings.NewReader(`{"name":"client","app":"site","ttl":24}`))
			require.NoError(t, err)
			resp, err := client.Do(req)
			require.NoError(t, err, "authorization must happen in the handler, not the TLS handshake")
			defer resp.Body.Close()
			body, err := io.ReadAll(resp.Body)
			require.NoError(t, err)
			assert.Equal(t, tc.status, resp.StatusCode, string(body))
			if tc.mode == authModeAudit {
				wantMessage := "certificate issuance authorization rejected caller"
				authorized := !tc.noCert && !tc.expired && len(tc.uris) == 1 && tc.uris[0] == siteManagerIdentity
				if authorized {
					wantMessage = "certificate issuance authorized caller"
				}
				found := false
				for _, entry := range hook.AllEntries() {
					if entry.Message == wantMessage {
						found = true
						assert.Equal(t, authModeAudit, entry.Data["authorization_mode"])
						if authorized {
							assert.Equal(t, siteManagerIdentity, entry.Data["service_identity"])
						} else {
							assert.NotEmpty(t, entry.Data["rejection_reason"])
						}
					}
				}
				assert.True(t, found, "AUDIT must record whether admission succeeded or failed")
			}
			if tc.path != "" || tc.status == 401 || tc.status == 403 {
				assert.Empty(t, issuer.requests)
			} else {
				require.Len(t, issuer.requests, 1)
			}
			if tc.status != 200 {
				assert.NotContains(t, string(body), "test-private-key")
			}
		})
	}
}

func TestNewCertificateAuthorizer(t *testing.T) {
	ca := newTestAuthority(t)
	issuer := ca.issuer(t)
	for _, tc := range []struct {
		name      string
		options   Options
		wantError string
	}{
		{name: "enforce needs allowlist", options: Options{AuthorizationMode: authModeEnforce}, wantError: "ENFORCE requires"},
		{name: "invalid mode", options: Options{AuthorizationMode: "enforce"}, wantError: "authorization mode"},
		{name: "malformed identity", options: Options{AllowedIdentities: []string{"https://example.test"}}, wantError: "invalid SPIFFE"},
		{name: "pathless identity", options: Options{AllowedIdentities: []string{"spiffe://nico.test"}}, wantError: "invalid SPIFFE"},
		{name: "duplicate identity", options: Options{AllowedIdentities: []string{siteManagerIdentity, siteManagerIdentity}}, wantError: "duplicate SPIFFE"},
		{name: "missing client CA", options: Options{ClientCAFile: filepath.Join(t.TempDir(), "missing")}, wantError: "read client CA"},
		{name: "old chart defaults to audit", options: Options{DNSNames: []string{"localhost"}}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			authorizer, err := newCertificateAuthorizer(context.Background(), tc.options, issuer)
			if tc.wantError != "" {
				require.ErrorContains(t, err, tc.wantError)
				assert.Nil(t, authorizer)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, authModeAudit, authorizer.mode)
		})
	}
}
