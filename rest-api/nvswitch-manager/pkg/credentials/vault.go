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

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"

	vault "github.com/hashicorp/vault/api"
	log "github.com/sirupsen/logrus"
)

// The mount path for the secrets engine
const mountPath = "secrets"

// BMC credentials are stored at machines/bmc/{bmc_mac}/root
const bmcCredentialPath = mountPath + "/data/machines/bmc"
const bmcCredentialSuffix = "root"

// NVOS credentials are stored at switch_nvos/{bmc_mac}/admin.
const nvosCredentialPath = mountPath + "/data/switch_nvos"
const nvosCredentialSuffix = "admin"

// errCredentialNotFound is the sentinel returned by get when a vault path
// holds no secret. It lets Put* distinguish "definitely absent → safe to
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
	return fmt.Sprintf("Vault Address: %s; Valid Vault Token: %t", c.Address, len(c.Token) > 0)
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
func (m *VaultCredentialManager) getBMCCredentialKey(mac net.HardwareAddr) string {
	return fmt.Sprintf("%s/%s/%s", bmcCredentialPath, strings.ToUpper(mac.String()), bmcCredentialSuffix)
}

func (m *VaultCredentialManager) getNVOSCredentialKey(mac net.HardwareAddr) string {
	return fmt.Sprintf("%s/%s/%s", nvosCredentialPath, strings.ToUpper(mac.String()), nvosCredentialSuffix)
}

func (m *VaultCredentialManager) get(ctx context.Context, key string) (*credential.Credential, error) {
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

func (m *VaultCredentialManager) put(ctx context.Context, key string, cred *credential.Credential) error {
	if cred == nil || !cred.IsValid() {
		return fmt.Errorf("valid credential not specified to Vault Manager")
	}

	payload := map[string]any{
		"data": credentialToMap(cred),
	}

	_, err := m.client.Logical().Write(key, payload)
	return err
}

func (m *VaultCredentialManager) delete(ctx context.Context, key string) error {
	_, err := m.client.Logical().Delete(key)
	return err
}

func credentialToMap(cred *credential.Credential) map[string]interface{} {
	if cred == nil {
		return nil
	}
	return map[string]interface{}{
		"UsernamePassword": map[string]interface{}{
			"username": cred.User,
			"password": cred.Password.Value,
		},
	}
}

func credentialFromMap(data map[string]interface{}) (*credential.Credential, error) {
	nested, ok := data["UsernamePassword"].(map[string]interface{})
	if !ok {
		return nil, errors.New("missing or invalid UsernamePassword field")
	}
	user, ok := nested["username"].(string)
	if !ok {
		return nil, errors.New("invalid username value")
	}
	password, ok := nested["password"].(string)
	if !ok {
		return nil, errors.New("invalid password value")
	}
	c := credential.New(user, password)
	return &c, nil
}

// GetBMC retrieves and validates BMC credentials for the given MAC from Vault.
func (m *VaultCredentialManager) GetBMC(ctx context.Context, mac net.HardwareAddr) (*credential.Credential, error) {
	return m.get(ctx, m.getBMCCredentialKey(mac))
}

// shouldSkipWrite reads the existing credential at key and decides whether
// the caller can short-circuit the write. Returns true only when an identical
// credential is already stored (idempotent re-register). For the differ and
// read-failure cases it logs a warning and returns false so the caller
// proceeds with an unconditional overwrite — matching the in-memory
// CredentialManager's upsert semantics.
//
// kind is "BMC" or "NVOS" purely for log readability; mac is logged as the
// MAC of the registering device.
func (m *VaultCredentialManager) shouldSkipWrite(ctx context.Context, key, kind string, mac net.HardwareAddr, cred *credential.Credential) bool {
	existing, err := m.get(ctx, key)
	switch {
	case err == nil && existing.Equal(cred):
		log.Infof("%s credentials for %s already exist and match; skipping write", kind, mac)
		return true
	case err == nil:
		log.Warnf("%s credentials for %s differ from existing; overwriting vault entry", kind, mac)
	case errors.Is(err, errCredentialNotFound):
		// No existing secret; fall through to write.
	default:
		log.Warnf("%s credentials for %s could not be read (%v); overwriting vault entry", kind, mac, err)
	}
	return false
}

// PutBMC writes BMC credentials to Vault. If an identical entry exists this
// is a no-op; if a different entry exists it is overwritten with a warning;
// if the existing entry could not be read (transient Vault failure, corrupted
// secret, etc.) the write proceeds with a warning so credential rotation is
// not blocked. Use PatchBMC for the same effect without the existence check.
func (m *VaultCredentialManager) PutBMC(ctx context.Context, mac net.HardwareAddr, cred *credential.Credential) error {
	key := m.getBMCCredentialKey(mac)
	if m.shouldSkipWrite(ctx, key, "BMC", mac, cred) {
		return nil
	}
	return m.put(ctx, key, cred)
}

// PatchBMC unconditionally replaces the BMC credentials in Vault.
func (m *VaultCredentialManager) PatchBMC(ctx context.Context, mac net.HardwareAddr, cred *credential.Credential) error {
	return m.put(ctx, m.getBMCCredentialKey(mac), cred)
}

// DeleteBMC removes the BMC credential from Vault.
func (m *VaultCredentialManager) DeleteBMC(ctx context.Context, mac net.HardwareAddr) error {
	return m.delete(ctx, m.getBMCCredentialKey(mac))
}

// GetNVOS retrieves and validates NVOS credentials for the given MAC from Vault.
func (m *VaultCredentialManager) GetNVOS(ctx context.Context, mac net.HardwareAddr) (*credential.Credential, error) {
	return m.get(ctx, m.getNVOSCredentialKey(mac))
}

// PutNVOS writes NVOS credentials to Vault. See PutBMC for the precise
// upsert semantics (match → skip, differ → warn-and-overwrite,
// unreadable → warn-and-overwrite).
func (m *VaultCredentialManager) PutNVOS(ctx context.Context, mac net.HardwareAddr, cred *credential.Credential) error {
	key := m.getNVOSCredentialKey(mac)
	if m.shouldSkipWrite(ctx, key, "NVOS", mac, cred) {
		return nil
	}
	return m.put(ctx, key, cred)
}

// PatchNVOS unconditionally replaces the NVOS credentials in Vault.
func (m *VaultCredentialManager) PatchNVOS(ctx context.Context, mac net.HardwareAddr, cred *credential.Credential) error {
	return m.put(ctx, m.getNVOSCredentialKey(mac), cred)
}

// DeleteNVOS removes the NVOS credential from Vault.
func (m *VaultCredentialManager) DeleteNVOS(ctx context.Context, mac net.HardwareAddr) error {
	return m.delete(ctx, m.getNVOSCredentialKey(mac))
}

// Keys returns a list of MACs for which the credential manager has BMC secrets for.
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
