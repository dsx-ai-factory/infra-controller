// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cli

import (
	"fmt"
	"os"
	"path/filepath"

	"gopkg.in/yaml.v3"
)

// ConfigFile mirrors the ~/.nico/config.yaml structure.
type ConfigFile struct {
	API  ConfigAPI  `yaml:"api"`
	Auth ConfigAuth `yaml:"auth"`
}

type ConfigAPI struct {
	Base string `yaml:"base,omitempty"`
	Org  string `yaml:"org,omitempty"`
	Name string `yaml:"name,omitempty"`
}

type ConfigAuth struct {
	Token        string        `yaml:"token,omitempty"`
	TokenCommand string        `yaml:"token_command,omitempty"`
	OIDC         *ConfigOIDC   `yaml:"oidc,omitempty"`
	APIKey       *ConfigAPIKey `yaml:"api_key,omitempty"`
}

// ConfigOIDC holds the OIDC login settings. Realm is consulted only when the token
// endpoint is built from --keycloak-url, because TokenURL already contains the realm.
type ConfigOIDC struct {
	TokenURL         string            `yaml:"token_url,omitempty"`
	Realm            string            `yaml:"realm,omitempty"`
	ClientID         string            `yaml:"client_id,omitempty"`
	ClientSecret     string            `yaml:"client_secret,omitempty"`
	ClientAuthMethod string            `yaml:"client_auth_method,omitempty"`
	Scopes           []string          `yaml:"scopes,omitempty"`
	TokenParameters  map[string]string `yaml:"token_parameters,omitempty"`
	Username         string            `yaml:"username,omitempty"`
	Password         string            `yaml:"password,omitempty"`
	Token            string            `yaml:"token,omitempty"`
	RefreshToken     string            `yaml:"refresh_token,omitempty"`
	ExpiresAt        string            `yaml:"expires_at,omitempty"`
}

type ConfigAPIKey struct {
	AuthnURL string `yaml:"authn_url,omitempty"`
	Key      string `yaml:"key,omitempty"`
	Token    string `yaml:"token,omitempty"`
}

var configOverridePath string

// SetConfigPath overrides the default config file path for the process lifetime.
func SetConfigPath(path string) {
	configOverridePath = path
}

// ConfigPath returns the active config file path.
func ConfigPath() string {
	if configOverridePath != "" {
		return configOverridePath
	}
	return defaultConfigPath()
}

func defaultConfigPath() string {
	home, err := os.UserHomeDir()
	if err != nil || home == "" {
		fmt.Fprintln(os.Stderr, "Warning: could not determine home directory, using current directory for config")
		return filepath.Join(".nico", "config.yaml")
	}
	return filepath.Join(home, ".nico", "config.yaml")
}

// ConfigDir returns the directory containing the active config file.
func ConfigDir() string {
	return filepath.Dir(ConfigPath())
}

// LoadConfig reads config from the active path (override or default).
func LoadConfig() (*ConfigFile, error) {
	return LoadConfigFromPath(ConfigPath())
}

// LoadConfigFromPath reads a config file at a specific path.
func LoadConfigFromPath(path string) (*ConfigFile, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return &ConfigFile{}, nil
		}
		return nil, err
	}
	var cfg ConfigFile
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return nil, fmt.Errorf("parsing config %s: %w", path, err)
	}
	return &cfg, nil
}

// SaveConfig writes the config back to ConfigPath(), preserving unknown keys.
func SaveConfig(cfg *ConfigFile) error {
	return SaveConfigToPath(cfg, ConfigPath())
}

// SaveConfigToPath writes the config to a specific path, preserving any
// unknown keys the user may have manually added.
func SaveConfigToPath(cfg *ConfigFile, path string) error {
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0700); err != nil {
		return fmt.Errorf("creating config directory: %w", err)
	}

	// Load existing file as raw map to preserve unknown keys.
	raw := make(map[string]interface{})
	if existing, err := os.ReadFile(path); err == nil {
		if err := yaml.Unmarshal(existing, &raw); err != nil {
			return fmt.Errorf("parsing existing config %s: %w", path, err)
		}
	}

	// Marshal the struct and merge into the raw map.
	structured, err := yaml.Marshal(cfg)
	if err != nil {
		return fmt.Errorf("marshaling config: %w", err)
	}
	var cfgMap map[string]interface{}
	if err := yaml.Unmarshal(structured, &cfgMap); err != nil {
		return fmt.Errorf("parsing marshaled config: %w", err)
	}
	for k, v := range cfgMap {
		raw[k] = v
	}

	data, err := yaml.Marshal(raw)
	if err != nil {
		return fmt.Errorf("marshaling config: %w", err)
	}
	return os.WriteFile(path, data, 0600)
}

// GetAuthToken returns the best available bearer token from the config.
// Priority: auth.token > auth.oidc.token > auth.api_key.token
func GetAuthToken(cfg *ConfigFile) string {
	if cfg.Auth.Token != "" {
		return cfg.Auth.Token
	}
	if cfg.Auth.OIDC != nil && cfg.Auth.OIDC.Token != "" {
		return cfg.Auth.OIDC.Token
	}
	if cfg.Auth.APIKey != nil && cfg.Auth.APIKey.Token != "" {
		return cfg.Auth.APIKey.Token
	}
	return ""
}

// HasOIDCConfig returns true when OIDC credentials are present in the config.
func HasOIDCConfig(cfg *ConfigFile) bool {
	return cfg.Auth.OIDC != nil &&
		cfg.Auth.OIDC.TokenURL != "" &&
		cfg.Auth.OIDC.ClientID != ""
}

// HasAPIKeyConfig returns true when NGC API key settings are present.
func HasAPIKeyConfig(cfg *ConfigFile) bool {
	return cfg.Auth.APIKey != nil &&
		cfg.Auth.APIKey.AuthnURL != "" &&
		cfg.Auth.APIKey.Key != ""
}

// HasTokenCommandConfig returns true when an auth token command is configured.
func HasTokenCommandConfig(cfg *ConfigFile) bool {
	return cfg.Auth.TokenCommand != ""
}

const SampleConfig = `# NICo CLI configuration
#
# API connection:
#   api.base -- server URL
#   api.org  -- organization name used in API paths
#   api.name -- API path segment (default: nico)
#
# Authentication options (choose one):
#   auth.token         -- direct bearer token (no login required)
#   auth.token_command -- shell command/script that prints a bearer token
#   auth.oidc          -- OIDC password/client-credentials flow
#   auth.api_key       -- NGC API key exchange
#
api:
  base: http://localhost:8388
  org: test-org
  name: nico

auth:
  # Option 1: Direct bearer token
  # token: eyJhbGciOi...

  # Option 2: Auth script/token command
  # token_command: /path/to/get-nico-token.sh

  # Option 3: OIDC provider (e.g. Keycloak)
  oidc:
    token_url: http://localhost:8080/realms/nico-dev/protocol/openid-connect/token
    client_id: nico-api
    client_secret: nico-local-secret
    # scopes: [openid]
    # client_auth_method: client_secret_post
    # token_parameters:
    #   audience: nico
    # Run 'nicocli login' to authenticate; it will prompt for username/password
    # and persist the resulting bearer token (and refresh token) here.

  # Option 4: NGC API key
  # api_key:
  #   key: nvapi-xxxx
  #   # authn_url is only required for legacy NGC keys (without nvapi- prefix)
  #   # authn_url: https://your-authn-server/token
`
