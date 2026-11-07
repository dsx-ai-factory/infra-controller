// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package credentials

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"net"
	"net/http"
	"strings"

	vault "github.com/hashicorp/vault/api"
	log "github.com/sirupsen/logrus"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
)

// The mount path for the secrets engine
const mountPath = "secrets"

// PMC credentials use the same KV layout as Core site explorer and NSM:
// secrets/data/machines/bmc/{mac}/root (mac uppercase; vault paths are case-sensitive).
const bmcCredentialPath = mountPath + "/data/machines/bmc"
const bmcCredentialSuffix = "root"

// errCredentialNotFound is the sentinel returned by Get when a vault path
// holds no secret. It lets Put distinguish "definitely absent → safe to
// write" from "couldn't determine state → log and write anyway", which is
// important to avoid silently overwriting on transient Vault read failures.
var errCredentialNotFound = errors.New("credential not found")

// VaultConfig configures access to Vault (address and token). The token should be scoped minimally for KV operations.
type VaultConfig struct {
	Address string
	Token   string
}

// String returns the canonical string form of the version.
func (c VaultConfig) String() string {
	return fmt.Sprintf("Vault Address: %s; Vault Token: %s", c.Address, c.Token)
}

// Validate ensures required Vault fields are provided.
func (c *VaultConfig) Validate() error {
	if strings.TrimSpace(c.Address) == "" {
		return errors.New("invalid vault address specified")
	}

	if strings.TrimSpace(c.Token) == "" {
		return errors.New("invalid vault token specified")
	}

	return nil
}

// VaultCredentialManager implements the CredentialManager interface with a Vault store.
type VaultCredentialManager struct {
	client *vault.Client
}

// NewManager initializes a Vault client with the configured address and token.
// TLS verification is skipped to handle self-signed certificates in Kubernetes environments.
func (c *VaultConfig) NewManager() (*VaultCredentialManager, error) {
	config := &vault.Config{
		Address: c.Address,
		HttpClient: &http.Client{
			Transport: &http.Transport{
				TLSClientConfig: &tls.Config{
					InsecureSkipVerify: true, //nolint:gosec // Skip TLS verify for internal K8s services
				},
			},
		},
	}
	client, err := vault.NewClient(config)
	if err != nil {
		return nil, err
	}

	client.SetToken(c.Token)

	return &VaultCredentialManager{
		client: client,
	}, nil
}

func (m *VaultCredentialManager) pathExists(path string) (bool, error) {
	mounts, err := m.client.Sys().ListMounts()
	if err != nil {
		return false, err
	}

	for mountPath := range mounts {
		if mountPath == path || mountPath == path+"/" {
			return true, nil
		}
	}
	return false, nil
}

func (m *VaultCredentialManager) configureVault() error {
	exists, err := m.pathExists(mountPath)
	if err != nil {
		return err
	}

	if exists {
		return nil
	}

	data := map[string]any{
		"type": "kv-v2",
	}
	_, err = m.client.Logical().Write(fmt.Sprintf("/sys/mounts/%s", mountPath), data)
	return err
}

// Start ensures the Vault engine is mounted at the configured path.
func (m *VaultCredentialManager) Start(ctx context.Context) error {
	log.Printf("Starting Vault credential manager")
	return m.configureVault()
}

// Stop performs no cleanup.
func (m *VaultCredentialManager) Stop(ctx context.Context) error {
	log.Printf("Stopping Vault credential manager")
	return nil
}

// Uppercase the MAC to match NICo Core's vault key convention (Rust's
// MacAddress Display trait emits uppercase hex). Go's net.HardwareAddr.String()
// emits lowercase, and vault paths are case-sensitive.
func (m *VaultCredentialManager) getCredentialKey(mac net.HardwareAddr) string {
	return fmt.Sprintf("%s/%s/%s", bmcCredentialPath, strings.ToUpper(mac.String()), bmcCredentialSuffix)
}

// Get retrieves and validates credentials for the given MAC from Vault.
func (m *VaultCredentialManager) Get(ctx context.Context, mac net.HardwareAddr) (*credential.Credential, error) {
	key := m.getCredentialKey(mac)
	secret, err := m.client.Logical().Read(key)
	if err != nil {
		return nil, fmt.Errorf("vault read at %q: %w", key, err)
	}
	if secret == nil || secret.Data == nil {
		return nil, fmt.Errorf("vault path %q: %w", key, errCredentialNotFound)
	}

	credData, ok := secret.Data["data"].(map[string]interface{})
	if !ok {
		return nil, fmt.Errorf("unexpected secret data format at vault path %q", key)
	}

	cred, err := credentialFromMap(credData)
	if err != nil {
		return nil, fmt.Errorf("parsing credential at vault path %q: %w", key, err)
	}

	if cred == nil || !cred.IsValid() {
		return nil, fmt.Errorf("retrieved invalid credential from vault path %q", key)
	}

	return cred, nil
}

// Put writes PMC credentials to Vault. If an identical entry exists this is
// a no-op; if a different entry exists it is overwritten with a warning;
// if the existing entry could not be read (transient Vault failure, corrupted
// secret, etc.) the write proceeds with a warning so credential rotation is
// not blocked. Use Patch for the same effect without the existence check.
func (m *VaultCredentialManager) Put(ctx context.Context, mac net.HardwareAddr, cred *credential.Credential) error {
	if cred == nil || !cred.IsValid() {
		return fmt.Errorf("valid credential not specified to Vault Manager")
	}

	existing, err := m.Get(ctx, mac)
	switch {
	case err == nil && existing.Equal(cred):
		log.Infof("PMC credentials for %s already exist and match; skipping write", mac)
		return nil
	case err == nil:
		log.Warnf("PMC credentials for %s differ from existing; overwriting vault entry", mac)
	case errors.Is(err, errCredentialNotFound):
		// No existing secret; fall through to write.
	default:
		log.Warnf("PMC credentials for %s could not be read (%v); overwriting vault entry", mac, err)
	}

	return m.write(ctx, mac, cred)
}

// Patch unconditionally replaces the PMC credentials in Vault.
func (m *VaultCredentialManager) Patch(ctx context.Context, mac net.HardwareAddr, cred *credential.Credential) error {
	if cred == nil || !cred.IsValid() {
		return fmt.Errorf("valid credential not specified to Vault Manager")
	}
	return m.write(ctx, mac, cred)
}

func (m *VaultCredentialManager) write(ctx context.Context, mac net.HardwareAddr, cred *credential.Credential) error {
	payload := map[string]any{
		"data": credentialToMap(cred),
	}

	key := m.getCredentialKey(mac)
	_, err := m.client.Logical().Write(key, payload)
	return err
}

// Delete removes the credential specified by the PMC mac (if it exists) from Vault.
func (m *VaultCredentialManager) Delete(ctx context.Context, mac net.HardwareAddr) error {
	key := m.getCredentialKey(mac)
	_, err := m.client.Logical().Delete(key)
	return err
}

// Keys returns a list of PMC MACs for which credential manager has BMC-root secrets.
// KV v2 lists live under metadata/
func (m *VaultCredentialManager) Keys(ctx context.Context) ([]net.HardwareAddr, error) {
	listPath := strings.Replace(bmcCredentialPath, "/data/", "/metadata/", 1)
	secret, err := m.client.Logical().List(listPath)
	if err != nil {
		return nil, err
	}
	if secret == nil || secret.Data == nil {
		return nil, errors.New("no credentials found")
	}

	keys, ok := secret.Data["keys"].([]interface{})
	if !ok {
		return nil, errors.New("unexpected data format")
	}

	macs := make([]net.HardwareAddr, 0, len(keys))
	for _, key := range keys {
		keyStr, ok := key.(string)
		if !ok {
			return nil, fmt.Errorf("unexpected key format: %v", key)
		}
		keyStr = strings.TrimSuffix(keyStr, "/")

		mac, err := net.ParseMAC(keyStr)
		if err != nil {
			continue
		}

		macs = append(macs, mac)
	}

	return macs, nil
}

var (
	errInvalidUsername = errors.New("invalid username value")
	errInvalidPassword = errors.New("invalid password value")
)

// credentialToMap converts a Credential to a map[string]interface{} suitable for Vault storage.
// Uses NICo's credential format with a UsernamePassword wrapper.
func credentialToMap(c *credential.Credential) map[string]interface{} {
	return map[string]interface{}{
		"UsernamePassword": map[string]interface{}{
			"username": c.User,
			"password": c.Password.Value,
		},
	}
}

// credentialFromMap converts a map[string]interface{} from Vault storage to a Credential.
// Expects NICo's credential format: {"UsernamePassword": {"username": ..., "password": ...}}
func credentialFromMap(data map[string]interface{}) (*credential.Credential, error) {
	nested, ok := data["UsernamePassword"].(map[string]interface{})
	if !ok {
		return nil, errors.New("missing or invalid UsernamePassword field")
	}

	user, ok := nested["username"].(string)
	if !ok {
		return nil, errInvalidUsername
	}

	password, ok := nested["password"].(string)
	if !ok {
		return nil, errInvalidPassword
	}

	cred := credential.New(user, password)
	return &cred, nil
}
