// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package grpcproxy_test

import (
	"context"
	"encoding/json"
	"os"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	proxytest "github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy/testutil"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/stretchr/testify/require"
)

func TestVaultProtector_Open(t *testing.T) {
	for _, tc := range []struct {
		name    string
		change  func(*grpcproxy.ProtectionContext, *grpcproxy.ProtectedPayload)
		wantErr bool
	}{
		{name: "serialized request survives whitespace and HTML escaping"},
		{name: "different site", change: func(c *grpcproxy.ProtectionContext, _ *grpcproxy.ProtectedPayload) { c.SiteID = "other" }, wantErr: true},
		{name: "different backend", change: func(c *grpcproxy.ProtectionContext, _ *grpcproxy.ProtectedPayload) {
			c.Service = grpcproxy.Flow.ServiceName
		}, wantErr: true},
		{name: "different method", change: func(c *grpcproxy.ProtectionContext, _ *grpcproxy.ProtectedPayload) { c.Method = "Other" }, wantErr: true},
		{name: "different workflow", change: func(c *grpcproxy.ProtectionContext, _ *grpcproxy.ProtectedPayload) { c.WorkflowID = "other" }, wantErr: true},
		{name: "different public request", change: func(c *grpcproxy.ProtectionContext, _ *grpcproxy.ProtectedPayload) { c.RequestHash = "other" }, wantErr: true},
		{name: "request substituted for response", change: func(c *grpcproxy.ProtectionContext, _ *grpcproxy.ProtectedPayload) { *c = c.Response() }, wantErr: true},
		{name: "unknown version", change: func(_ *grpcproxy.ProtectionContext, p *grpcproxy.ProtectedPayload) { p.Version++ }, wantErr: true},
		{name: "foreign key name", change: func(_ *grpcproxy.ProtectionContext, p *grpcproxy.ProtectedPayload) { p.KeyID = "other" }, wantErr: true},
		{name: "malformed ciphertext", change: func(_ *grpcproxy.ProtectionContext, p *grpcproxy.ProtectedPayload) { p.Ciphertext = "vault:v1:invalid" }, wantErr: true},
		{name: "modified context cannot be authenticated", change: func(c *grpcproxy.ProtectionContext, p *grpcproxy.ProtectedPayload) {
			c.SiteID = "other"
			p.Context = *c
		}, wantErr: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			proxytest.Configure(t)
			protector, err := grpcproxy.NewVaultProtectorFromEnvironment()
			require.NoError(t, err)
			req := grpcproxy.Request{FullMethod: "/forge.Forge/CreateCredential", RequestJSON: json.RawMessage(`{ "password": "[REDACTED]", "url": "https://firmware.local/image?a=1&b=2", "counter": 9007199254740993 }`)}
			binding := grpcproxy.NewProtectionContext("site-1", grpcproxy.Core, "workflow-1", req)
			req.ProtectedSecrets, err = protector.Seal(context.Background(), binding, []byte(`{"password":"fixture-secret"}`))
			require.NoError(t, err)
			encoded, err := json.Marshal(req)
			require.NoError(t, err)
			require.NotContains(t, string(encoded), "fixture-secret")
			var received grpcproxy.Request
			require.NoError(t, json.Unmarshal(encoded, &received))
			expected := grpcproxy.NewProtectionContext("site-1", grpcproxy.Core, "workflow-1", received)
			if tc.change != nil {
				tc.change(&expected, received.ProtectedSecrets)
			}
			plain, err := protector.Open(context.Background(), expected, received.ProtectedSecrets)
			if tc.wantErr {
				require.Error(t, err)
				require.Empty(t, plain)
				return
			}
			require.NoError(t, err)
			require.JSONEq(t, `{"password":"fixture-secret"}`, string(plain))
		})
	}
}

func TestVaultProtector_Seal(t *testing.T) {
	for _, tc := range []struct {
		name         string
		corruptToken bool
		replaceVault bool
	}{
		{name: "fresh encryption uses distinct nonces"},
		{name: "token file is reread and recovers after renewal", corruptToken: true},
		{name: "replacement Vault cannot decrypt old data", replaceVault: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			proxytest.Configure(t)
			p, err := grpcproxy.NewVaultProtectorFromEnvironment()
			require.NoError(t, err)
			binding := grpcproxy.NewProtectionContext("site", grpcproxy.Core, "workflow", grpcproxy.Request{FullMethod: "Test", RequestJSON: []byte(`{}`)})
			first, err := p.Seal(context.Background(), binding, []byte(`{"password":"secret"}`))
			require.NoError(t, err)
			if tc.corruptToken {
				path := os.Getenv("GRPC_PROXY_VAULT_TOKEN_FILE")
				require.NoError(t, os.WriteFile(path, []byte("expired-token"), 0600))
				_, err = p.Seal(context.Background(), binding, []byte(`{}`))
				require.ErrorIs(t, err, grpcproxy.ErrProtectionUnavailable)
				require.NoError(t, os.WriteFile(path, []byte("fixture-token"), 0600))
			}
			if tc.replaceVault {
				proxytest.Configure(t)
				p, err = grpcproxy.NewVaultProtectorFromEnvironment()
				require.NoError(t, err)
				_, err = p.Open(context.Background(), binding, first)
				require.ErrorIs(t, err, grpcproxy.ErrProtectionUnavailable)
				return
			}
			second, err := p.Seal(context.Background(), binding, []byte(`{"password":"secret"}`))
			require.NoError(t, err)
			require.NotEqual(t, first.Ciphertext, second.Ciphertext)
			plain, err := p.Open(context.Background(), binding, first)
			require.NoError(t, err)
			require.JSONEq(t, `{"password":"secret"}`, string(plain))
		})
	}
}

func TestNewVaultProtectorFromEnvironment(t *testing.T) {
	for _, tc := range []struct{ name, variable, value string }{
		{"missing address", "GRPC_PROXY_VAULT_ADDR", ""},
		{"plaintext transport", "GRPC_PROXY_VAULT_ADDR", "http://vault.local:8200"},
		{"URL userinfo", "GRPC_PROXY_VAULT_ADDR", "https://user:password@vault.local:8200"},
		{"unexpected path", "GRPC_PROXY_VAULT_ADDR", "https://vault.local:8200/v1"},
		{"missing token file", "GRPC_PROXY_VAULT_TOKEN_FILE", ""},
		{"unreadable CA", "GRPC_PROXY_VAULT_CA_FILE", "/missing-ca.crt"},
		{"path traversal", "GRPC_PROXY_VAULT_KEY", "../other-key"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			proxytest.Configure(t)
			t.Setenv(tc.variable, tc.value)
			_, err := grpcproxy.NewVaultProtectorFromEnvironment()
			require.ErrorIs(t, err, grpcproxy.ErrProtectionConfig)
			require.NotContains(t, err.Error(), "password")
		})
	}
}

func TestOpenLegacySecrets(t *testing.T) {
	for _, tc := range []struct {
		name       string
		ciphertext []byte
		siteID     string
		wantErr    bool
	}{
		{"predecessor ciphertext", cutil.EncryptData([]byte(`{"password":"legacy"}`), "site"), "site", false},
		{"wrong site", cutil.EncryptData([]byte(`{}`), "site"), "other", true},
		{"truncated history", []byte{1, 2, 3}, "site", true},
		{"missing site", cutil.EncryptData([]byte(`{}`), "site"), "", true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			plain, err := grpcproxy.OpenLegacySecrets(tc.ciphertext, tc.siteID)
			if tc.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			require.JSONEq(t, `{"password":"legacy"}`, string(plain))
		})
	}
}
