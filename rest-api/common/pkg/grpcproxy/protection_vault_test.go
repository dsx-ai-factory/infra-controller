//go:build vault_integration

// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package grpcproxy_test

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	vault "github.com/hashicorp/vault/api"
	"github.com/stretchr/testify/require"
)

// This explicit build-tag test targets a disposable, loopback-only Vault dev
// server with root token fixture-token. It never uses the host's Vault identity.
func TestVaultProtector_TransitRotation(t *testing.T) {
	address := os.Getenv("GRPC_PROXY_VAULT_ADDR")
	u, err := url.Parse(address)
	require.NoError(t, err)
	require.Equal(t, "https", u.Scheme)
	require.Contains(t, []string{"127.0.0.1", "localhost"}, u.Hostname())
	require.Empty(t, os.Getenv("GRPC_PROXY_VAULT_NAMESPACE"))
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	// Poison ambient routing settings to prove setup cannot select a host
	// Vault Agent or identity instead of the validated disposable listener.
	t.Setenv("VAULT_AGENT_ADDR", "http://127.0.0.1:1")
	t.Setenv("VAULT_ADDR", "http://127.0.0.1:1")
	ca, err := os.ReadFile(os.Getenv("GRPC_PROXY_VAULT_CA_FILE"))
	require.NoError(t, err)
	roots := x509.NewCertPool()
	require.True(t, roots.AppendCertsFromPEM(ca))
	config := &vault.Config{Address: address, DisableRedirects: true, Timeout: 5 * time.Second,
		HttpClient: &http.Client{Timeout: 5 * time.Second,
			Transport:     &http.Transport{DisableKeepAlives: true, TLSClientConfig: &tls.Config{RootCAs: roots, MinVersion: tls.VersionTLS12}},
			CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}}
	admin, err := vault.NewClient(config)
	require.NoError(t, err)
	admin.SetNamespace("")
	admin.SetToken("fixture-token")
	require.NoError(t, admin.Sys().MountWithContext(ctx, "transit", &vault.MountInput{Type: "transit"}))
	_, err = admin.Logical().WriteWithContext(ctx, "transit/keys/rest-proxy", map[string]interface{}{
		"type": "aes256-gcm96", "derived": true, "exportable": false,
	})
	require.NoError(t, err)
	require.NoError(t, admin.Sys().PutPolicyWithContext(ctx, "proxy", `
path "transit/encrypt/rest-proxy" { capabilities = ["update"] }
path "transit/decrypt/rest-proxy" { capabilities = ["update"] }
`))
	token, err := admin.Auth().Token().CreateWithContext(ctx, &vault.TokenCreateRequest{
		Policies: []string{"proxy"}, NoDefaultPolicy: true, TTL: "10m",
	})
	require.NoError(t, err)
	tokenFile := filepath.Join(t.TempDir(), "token")
	require.NoError(t, os.WriteFile(tokenFile, []byte(token.Auth.ClientToken), 0600))
	t.Setenv("GRPC_PROXY_VAULT_TOKEN_FILE", tokenFile)
	t.Setenv("GRPC_PROXY_VAULT_MOUNT", "transit")
	t.Setenv("GRPC_PROXY_VAULT_KEY", "rest-proxy")
	p, err := grpcproxy.NewVaultProtectorFromEnvironment()
	require.NoError(t, err)
	binding := grpcproxy.NewProtectionContext("site", grpcproxy.Core, "workflow", grpcproxy.Request{FullMethod: "Test", RequestJSON: []byte(`{}`)})
	first, err := p.Seal(ctx, binding, []byte(`{"password":"rotation-fixture"}`))
	require.NoError(t, err)
	require.Contains(t, first.Ciphertext, "vault:v1:")
	_, err = admin.Logical().WriteWithContext(ctx, "transit/keys/rest-proxy/rotate", nil)
	require.NoError(t, err)
	second, err := p.Seal(ctx, binding, []byte(`{"password":"rotation-fixture"}`))
	require.NoError(t, err)
	require.Contains(t, second.Ciphertext, "vault:v2:")
	plain, err := p.Open(ctx, binding, first)
	require.NoError(t, err)
	require.JSONEq(t, `{"password":"rotation-fixture"}`, string(plain))
	// Alter both public copies of the binding. Only authenticated Vault context
	// and AAD can reject this relocation; struct equality alone cannot.
	relocated := *first
	relocated.Context.SiteID = "other-site"
	_, err = p.Open(ctx, relocated.Context, &relocated)
	require.ErrorIs(t, err, grpcproxy.ErrProtectionUnavailable)
	_, err = admin.Logical().WriteWithContext(ctx, "transit/keys/rest-proxy/config", map[string]interface{}{"min_decryption_version": 2})
	require.NoError(t, err)
	_, err = p.Open(ctx, binding, first)
	require.ErrorIs(t, err, grpcproxy.ErrProtectionUnavailable)
	plain, err = p.Open(ctx, binding, second)
	require.NoError(t, err)
	require.JSONEq(t, `{"password":"rotation-fixture"}`, string(plain))
	_, err = admin.Logical().WriteWithContext(ctx, "transit/keys/rest-proxy/config", map[string]interface{}{"min_decryption_version": 1})
	require.NoError(t, err)
	// A fresh service instance with restored token/CA configuration can still
	// decrypt retained versions; this is not a Vault snapshot/restore test.
	restarted, err := grpcproxy.NewVaultProtectorFromEnvironment()
	require.NoError(t, err)
	plain, err = restarted.Open(ctx, binding, first)
	require.NoError(t, err)
	require.JSONEq(t, `{"password":"rotation-fixture"}`, string(plain))
}
