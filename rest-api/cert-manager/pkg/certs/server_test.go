// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package certs

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"fmt"
	"sync"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/cert-manager/pkg/core"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

type renewingCertificateIssuer struct {
	CertificateIssuer
	calls     int
	fail      bool
	malformed bool
}

func (i *renewingCertificateIssuer) RawCertificate(ctx context.Context, sans []string, ttl int) (string, string, error) {
	i.calls++
	if i.fail {
		return "", "", fmt.Errorf("temporary signing failure")
	}
	if i.malformed {
		return "invalid certificate", "invalid key", nil
	}
	return i.CertificateIssuer.RawCertificate(ctx, sans, ttl)
}

func TestServer_getCertificate(t *testing.T) {
	for _, tc := range []struct {
		name                            string
		renew, fail, expired, malformed bool
		clients                         int
	}{
		{name: "reuse current certificate", clients: 1},
		{name: "renew on concurrent verified handshakes", renew: true, clients: 8},
		{name: "retain valid certificate and throttle failed renewal", renew: true, fail: true, clients: 2},
		{name: "reject expired certificate after renewal failure", renew: true, fail: true, expired: true, clients: 2},
		{name: "retain valid certificate after invalid renewal", renew: true, malformed: true, clients: 2},
	} {
		t.Run(tc.name, func(t *testing.T) {
			registerer := prometheus.DefaultRegisterer
			prometheus.DefaultRegisterer = prometheus.NewRegistry()
			t.Cleanup(func() { prometheus.DefaultRegisterer = registerer })
			ca := newTestAuthority(t)
			issuer := &renewingCertificateIssuer{CertificateIssuer: ca.issuer(t)}
			logCtx, hook := core.WithTestingLogger(context.Background())
			ctx, cancel := context.WithCancel(logCtx)
			defer cancel()
			server, err := NewServerWithIssuer(ctx, Options{Addr: "127.0.0.1:0", DNSNames: []string{"localhost"}}, issuer)
			require.NoError(t, err)
			initial := server.servingCertificate.Certificate[0]
			if tc.renew {
				server.nextRenewal = time.Now().Add(-time.Second)
			}
			if tc.expired {
				server.servingCertificate.Leaf.NotAfter = time.Now().Add(-time.Second)
			}
			issuer.fail, issuer.malformed = tc.fail, tc.malformed
			listener, err := server.appService.Start(ctx)
			require.NoError(t, err)
			t.Cleanup(func() { cancel(); <-server.appService.Done() })
			roots := x509.NewCertPool()
			require.True(t, roots.AppendCertsFromPEM(ca.pem))
			type result struct {
				cert []byte
				err  error
			}
			results := make(chan result, tc.clients)
			var wg sync.WaitGroup
			for range tc.clients {
				wg.Go(func() {
					conn, dialErr := tls.Dial("tcp", listener.Addr().String(), &tls.Config{RootCAs: roots, ServerName: "localhost", MinVersion: tls.VersionTLS12})
					if dialErr != nil {
						results <- result{err: dialErr}
						return
					}
					results <- result{cert: conn.ConnectionState().PeerCertificates[0].Raw}
					_ = conn.Close()
				})
			}
			wg.Wait()
			close(results)
			for got := range results {
				if tc.expired {
					assert.Error(t, got.err)
					continue
				}
				require.NoError(t, got.err)
				if tc.renew && !tc.fail && !tc.malformed {
					assert.NotEqual(t, initial, got.cert)
				} else {
					assert.Equal(t, initial, got.cert)
				}
			}
			server.certificateMu.Lock()
			calls := issuer.calls
			server.certificateMu.Unlock()
			if tc.renew {
				assert.Equal(t, 2, calls)
			} else {
				assert.Equal(t, 1, calls)
			}
			if tc.fail || tc.malformed {
				require.NotNil(t, hook.LastEntry())
				assert.Equal(t, "failed to renew serving certificate", hook.LastEntry().Message)
			}
			if tc.fail {
				// Simulate the retry deadline; a later successful signing recovers.
				issuer.fail = false
				server.nextRenewal = time.Now().Add(-time.Second)
				conn, dialErr := tls.Dial("tcp", listener.Addr().String(), &tls.Config{RootCAs: roots, ServerName: "localhost", MinVersion: tls.VersionTLS12})
				require.NoError(t, dialErr)
				assert.NotEqual(t, initial, conn.ConnectionState().PeerCertificates[0].Raw)
				require.NoError(t, conn.Close())
			}
		})
	}
}
