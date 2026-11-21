// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package grpcproxy

import (
	"bytes"
	"context"
	"crypto/aes"
	"crypto/cipher"
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"net/http"
	"net/url"
	"os"
	"regexp"
	"strings"
	"time"

	vault "github.com/hashicorp/vault/api"
)

var (
	// ErrProtectionConfig indicates absent or invalid local Vault configuration.
	ErrProtectionConfig = errors.New("gRPC proxy Vault configuration is missing or invalid")
	// ErrProtectionUnavailable indicates a failed Vault operation. Upstream error
	// text is deliberately excluded because it can contain sensitive data.
	ErrProtectionUnavailable = errors.New("gRPC proxy Vault operation failed")
	// ErrInvalidProtection indicates an invalid envelope or context mismatch.
	ErrInvalidProtection = errors.New("invalid gRPC proxy protected payload")
	transitName          = regexp.MustCompile(`^[a-zA-Z0-9_-]+$`)
)

// ProtectionContext binds secret fields and replies to one site, service,
// method, workflow ID and readable request. Direction is "request" or "response".
// It is Transit key-derivation context and AEAD associated data, not a secret
// or an authorization token.
type ProtectionContext struct {
	SiteID      string `json:"siteId"`
	Service     string `json:"service"`
	Method      string `json:"method"`
	WorkflowID  string `json:"workflowId"`
	RequestHash string `json:"requestHash"`
	Direction   string `json:"direction"`
}

// NewProtectionContext binds a request to trusted routing and execution identity.
func NewProtectionContext(siteID string, backend Backend, workflowID string, req Request) ProtectionContext {
	// Temporal's outer JSON encoding can change whitespace and HTML escaping.
	// Preserve number precision while hashing the semantic JSON representation.
	canonical := req.RequestJSON
	decoder := json.NewDecoder(bytes.NewReader(req.RequestJSON))
	decoder.UseNumber()
	var value interface{}
	if decoder.Decode(&value) == nil {
		encoded, err := json.Marshal(value)
		if err == nil {
			canonical = encoded
		}
	}
	digest := sha256.Sum256(canonical)
	return ProtectionContext{SiteID: siteID, Service: backend.ServiceName,
		Method: req.FullMethod, WorkflowID: workflowID, RequestHash: hex.EncodeToString(digest[:]), Direction: "request"}
}

// Response returns the matching reply context, preventing request/reply substitution.
func (c ProtectionContext) Response() ProtectionContext {
	c.Direction = "response"
	return c
}

// ProtectedPayload is version 1 of the persisted Transit envelope. KeyID names
// the configured Transit key; Ciphertext includes Vault's key version. Context
// is checked against trusted execution identity before any decryption request.
type ProtectedPayload struct {
	Version    int               `json:"version"`
	KeyID      string            `json:"keyId"`
	Context    ProtectionContext `json:"context"`
	Ciphertext string            `json:"ciphertext"`
}

// VaultProtector uses the site's existing Vault authentication facilities.
// The token file is reread for every operation so an external Vault Agent or
// Secret controller can renew/replace it without restarting the service.
type VaultProtector struct {
	client    *vault.Client
	tokenFile string
	key       string
	mount     string
}

// NewVaultProtectorFromEnvironment configures HTTPS Transit from
// GRPC_PROXY_VAULT_ADDR, GRPC_PROXY_VAULT_TOKEN_FILE and GRPC_PROXY_VAULT_CA_FILE.
// GRPC_PROXY_VAULT_MOUNT defaults to transit; GRPC_PROXY_VAULT_KEY defaults to
// rest-proxy. Both must be single path segments. GRPC_PROXY_VAULT_NAMESPACE is
// optional. No environment-based TLS-verification bypass is supported.
func NewVaultProtectorFromEnvironment() (*VaultProtector, error) {
	address := os.Getenv("GRPC_PROXY_VAULT_ADDR")
	tokenFile := os.Getenv("GRPC_PROXY_VAULT_TOKEN_FILE")
	caFile := os.Getenv("GRPC_PROXY_VAULT_CA_FILE")
	u, err := url.Parse(address)
	if err != nil || u.Scheme != "https" || u.Host == "" || u.User != nil || u.RawQuery != "" || u.Fragment != "" || (u.Path != "" && u.Path != "/") || tokenFile == "" || caFile == "" {
		return nil, ErrProtectionConfig
	}
	mount, key := os.Getenv("GRPC_PROXY_VAULT_MOUNT"), os.Getenv("GRPC_PROXY_VAULT_KEY")
	if mount == "" {
		mount = "transit"
	}
	if key == "" {
		key = "rest-proxy"
	}
	if !transitName.MatchString(mount) || !transitName.MatchString(key) {
		return nil, ErrProtectionConfig
	}
	ca, err := os.ReadFile(caFile)
	if err != nil {
		return nil, ErrProtectionConfig
	}
	roots := x509.NewCertPool()
	if !roots.AppendCertsFromPEM(ca) {
		return nil, ErrProtectionConfig
	}
	client, err := vault.NewClient(&vault.Config{Address: address, MaxRetries: 0, DisableRedirects: true,
		Timeout: 5 * time.Second, HttpClient: &http.Client{Timeout: 5 * time.Second,
			Transport:     &http.Transport{DisableKeepAlives: true, TLSClientConfig: &tls.Config{RootCAs: roots, MinVersion: tls.VersionTLS12}},
			CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}})
	if err != nil {
		return nil, ErrProtectionConfig
	}
	client.SetNamespace(os.Getenv("GRPC_PROXY_VAULT_NAMESPACE"))
	return &VaultProtector{client: client, tokenFile: tokenFile, mount: mount, key: key}, nil
}

// Seal encrypts JSON secret fields or a reply using Vault-managed nonces and
// a derived aes256-gcm96 Transit key. It never creates or rotates a key.
func (p *VaultProtector) Seal(ctx context.Context, binding ProtectionContext, plaintext []byte) (*ProtectedPayload, error) {
	// Empty gRPC replies have the same JSON object representation as Empty.
	if len(plaintext) == 0 {
		plaintext = []byte(`{}`)
	}
	data, err := p.write(ctx, "encrypt", binding, "plaintext", base64.StdEncoding.EncodeToString(plaintext))
	if err != nil {
		return nil, err
	}
	ciphertext, ok := data["ciphertext"].(string)
	if !ok || !strings.HasPrefix(ciphertext, "vault:v") {
		return nil, ErrProtectionUnavailable
	}
	return &ProtectedPayload{Version: 1, KeyID: p.key, Context: binding, Ciphertext: ciphertext}, nil
}

// Open rejects foreign envelope versions, keys and contexts before calling Vault.
// Old Transit key versions remain decryptable only while Vault retains and permits them.
func (p *VaultProtector) Open(ctx context.Context, binding ProtectionContext, payload *ProtectedPayload) ([]byte, error) {
	if payload == nil || payload.Version != 1 || payload.KeyID != p.key || payload.Context != binding || !strings.HasPrefix(payload.Ciphertext, "vault:v") {
		return nil, ErrInvalidProtection
	}
	data, err := p.write(ctx, "decrypt", binding, "ciphertext", payload.Ciphertext)
	if err != nil {
		return nil, err
	}
	encoded, ok := data["plaintext"].(string)
	if !ok {
		return nil, ErrProtectionUnavailable
	}
	plaintext, err := base64.StdEncoding.DecodeString(encoded)
	if err != nil {
		return nil, ErrProtectionUnavailable
	}
	return plaintext, nil
}

func (p *VaultProtector) write(ctx context.Context, operation string, binding ProtectionContext, field, value string) (map[string]interface{}, error) {
	if binding.SiteID == "" || binding.Service == "" || binding.Method == "" || binding.WorkflowID == "" || binding.RequestHash == "" || (binding.Direction != "request" && binding.Direction != "response") {
		return nil, ErrInvalidProtection
	}
	token, err := os.ReadFile(p.tokenFile)
	if err != nil || strings.TrimSpace(string(token)) == "" {
		return nil, ErrProtectionConfig
	}
	// Each protector belongs to one invocation; no shared client token mutation.
	p.client.SetToken(strings.TrimSpace(string(token)))
	encodedContext, err := json.Marshal(struct {
		Domain string `json:"domain"`
		ProtectionContext
	}{Domain: "nico-grpc-proxy/v1", ProtectionContext: binding})
	if err != nil {
		return nil, ErrInvalidProtection
	}
	callCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	encoded := base64.StdEncoding.EncodeToString(encodedContext)
	secret, err := p.client.Logical().WriteWithContext(callCtx, p.mount+"/"+operation+"/"+p.key,
		map[string]interface{}{field: value, "context": encoded, "associated_data": encoded})
	if err != nil || secret == nil || secret.Data == nil {
		return nil, ErrProtectionUnavailable
	}
	return secret.Data, nil
}

// OpenLegacySecrets reads the predecessor's site-ID-derived AES-GCM format.
// Only migration code may call it; it does not establish confidentiality from
// readers who know the site ID. Malformed histories return errors, never panic.
func OpenLegacySecrets(ciphertext []byte, siteID string) ([]byte, error) {
	if siteID == "" {
		return nil, ErrInvalidProtection
	}
	key := sha256.Sum256([]byte(siteID))
	block, err := aes.NewCipher(key[:])
	if err != nil {
		return nil, ErrInvalidProtection
	}
	gcm, err := cipher.NewGCM(block)
	if err != nil || len(ciphertext) < gcm.NonceSize()+gcm.Overhead() {
		return nil, ErrInvalidProtection
	}
	plaintext, err := gcm.Open(nil, ciphertext[:gcm.NonceSize()], ciphertext[gcm.NonceSize():], nil)
	if err != nil {
		return nil, ErrInvalidProtection
	}
	return plaintext, nil
}
