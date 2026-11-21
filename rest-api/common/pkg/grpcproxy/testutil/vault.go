// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package testutil provides an HTTPS Transit protocol fixture. It exercises
// serialization, TLS, token-file refresh and authenticated context binding;
// it is not a substitute for verification against a real Vault deployment.
package testutil

import (
	"context"
	"crypto/aes"
	"crypto/cipher"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"encoding/pem"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	"github.com/stretchr/testify/require"
)

// Configure starts a fresh Transit protocol fixture and configures its HTTPS
// endpoint, CA and projected token file for the current test. Calls are serial.
func Configure(t *testing.T) {
	t.Helper()
	var key [32]byte
	_, err := rand.Read(key[:])
	require.NoError(t, err)
	server := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("X-Vault-Token") != "fixture-token" {
			http.Error(w, "denied", http.StatusForbidden)
			return
		}
		var data map[string]string
		if json.NewDecoder(r.Body).Decode(&data) != nil {
			http.Error(w, "invalid", http.StatusBadRequest)
			return
		}
		derived := sha256.Sum256(append(key[:], []byte(data["context"])...))
		block, blockErr := aes.NewCipher(derived[:])
		require.NoError(t, blockErr)
		gcm, gcmErr := cipher.NewGCM(block)
		require.NoError(t, gcmErr)
		result := map[string]string{}
		aad, aadErr := base64.StdEncoding.DecodeString(data["associated_data"])
		if aadErr != nil {
			http.Error(w, "invalid", http.StatusBadRequest)
			return
		}
		switch r.URL.Path {
		case "/v1/transit/encrypt/rest-proxy":
			plain, decodeErr := base64.StdEncoding.DecodeString(data["plaintext"])
			if decodeErr != nil {
				http.Error(w, "invalid", http.StatusBadRequest)
				return
			}
			nonce := make([]byte, gcm.NonceSize())
			_, readErr := rand.Read(nonce)
			require.NoError(t, readErr)
			result["ciphertext"] = "vault:v1:" + base64.StdEncoding.EncodeToString(gcm.Seal(nonce, nonce, plain, aad))
		case "/v1/transit/decrypt/rest-proxy":
			sealed, decodeErr := base64.StdEncoding.DecodeString(strings.TrimPrefix(data["ciphertext"], "vault:v1:"))
			if decodeErr != nil || len(sealed) < gcm.NonceSize()+gcm.Overhead() {
				http.Error(w, "invalid", http.StatusBadRequest)
				return
			}
			plain, openErr := gcm.Open(nil, sealed[:gcm.NonceSize()], sealed[gcm.NonceSize():], aad)
			if openErr != nil {
				http.Error(w, "invalid", http.StatusBadRequest)
				return
			}
			result["plaintext"] = base64.StdEncoding.EncodeToString(plain)
		default:
			http.NotFound(w, r)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]interface{}{"data": result})
	}))
	t.Cleanup(server.Close)
	dir := t.TempDir()
	caFile, tokenFile := filepath.Join(dir, "ca.crt"), filepath.Join(dir, "token")
	require.NoError(t, os.WriteFile(caFile, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: server.Certificate().Raw}), 0600))
	require.NoError(t, os.WriteFile(tokenFile, []byte("fixture-token"), 0600))
	for name, value := range map[string]string{
		"GRPC_PROXY_VAULT_ADDR": server.URL, "GRPC_PROXY_VAULT_CA_FILE": caFile,
		"GRPC_PROXY_VAULT_TOKEN_FILE": tokenFile, "GRPC_PROXY_VAULT_KEY": "rest-proxy",
		"GRPC_PROXY_VAULT_MOUNT": "transit", "GRPC_PROXY_VAULT_NAMESPACE": "",
	} {
		t.Setenv(name, value)
	}
}

// OpenSecrets decodes the request envelope as a site consumer would, with the
// expected site and workflow ID checked separately by the caller's assertions.
func OpenSecrets(t *testing.T, req grpcproxy.Request) []byte {
	t.Helper()
	require.NotNil(t, req.ProtectedSecrets)
	p, err := grpcproxy.NewVaultProtectorFromEnvironment()
	require.NoError(t, err)
	plain, err := p.Open(context.Background(), req.ProtectedSecrets.Context, req.ProtectedSecrets)
	require.NoError(t, err)
	return plain
}

// ProtectReply gives a mocked successful workflow the same encrypted result as
// the real proxy activity, retaining the exact protojson bytes.
func ProtectReply(t *testing.T, req grpcproxy.Request, out *grpcproxy.Response) {
	t.Helper()
	if req.ProtectedSecrets == nil {
		return
	}
	p, err := grpcproxy.NewVaultProtectorFromEnvironment()
	require.NoError(t, err)
	out.ProtectedResponse, err = p.Seal(context.Background(), req.ProtectedSecrets.Context.Response(), out.ResponseJSON)
	require.NoError(t, err, fmt.Sprintf("seal reply for %s", req.FullMethod))
	out.ResponseJSON = nil
}
