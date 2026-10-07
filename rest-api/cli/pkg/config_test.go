// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cli

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestGetAuthToken_Priority(t *testing.T) {
	tests := []struct {
		name string
		cfg  ConfigFile
		want string
	}{
		{
			name: "direct token wins",
			cfg: ConfigFile{
				Auth: ConfigAuth{
					Token:  "direct-token",
					OIDC:   &ConfigOIDC{Token: "oidc-token"},
					APIKey: &ConfigAPIKey{Token: "api-key-token"},
				},
			},
			want: "direct-token",
		},
		{
			name: "oidc token when no direct token",
			cfg: ConfigFile{
				Auth: ConfigAuth{
					OIDC:   &ConfigOIDC{Token: "oidc-token"},
					APIKey: &ConfigAPIKey{Token: "api-key-token"},
				},
			},
			want: "oidc-token",
		},
		{
			name: "api key token as last resort",
			cfg: ConfigFile{
				Auth: ConfigAuth{
					APIKey: &ConfigAPIKey{Token: "api-key-token"},
				},
			},
			want: "api-key-token",
		},
		{
			name: "empty when nothing configured",
			cfg:  ConfigFile{},
			want: "",
		},
		{
			name: "empty oidc token falls through to api key",
			cfg: ConfigFile{
				Auth: ConfigAuth{
					OIDC:   &ConfigOIDC{Token: ""},
					APIKey: &ConfigAPIKey{Token: "api-key-token"},
				},
			},
			want: "api-key-token",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := GetAuthToken(&tt.cfg)
			if got != tt.want {
				t.Errorf("GetAuthToken() = %q, want %q", got, tt.want)
			}
		})
	}
}

func TestHasTokenCommandConfig(t *testing.T) {
	cfg := ConfigFile{Auth: ConfigAuth{TokenCommand: "printf token"}}
	require.True(t, HasTokenCommandConfig(&cfg))
	require.False(t, HasTokenCommandConfig(&ConfigFile{}))
}

func TestHasOIDCConfig(t *testing.T) {
	tests := []struct {
		name string
		cfg  ConfigFile
		want bool
	}{
		{
			name: "fully configured",
			cfg: ConfigFile{
				Auth: ConfigAuth{
					OIDC: &ConfigOIDC{
						TokenURL: "https://auth.example.com/token",
						ClientID: "my-client",
					},
				},
			},
			want: true,
		},
		{
			name: "missing token url",
			cfg: ConfigFile{
				Auth: ConfigAuth{
					OIDC: &ConfigOIDC{ClientID: "my-client"},
				},
			},
			want: false,
		},
		{
			name: "nil oidc",
			cfg:  ConfigFile{},
			want: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := HasOIDCConfig(&tt.cfg)
			if got != tt.want {
				t.Errorf("HasOIDCConfig() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestHasAPIKeyConfig(t *testing.T) {
	tests := []struct {
		name string
		cfg  ConfigFile
		want bool
	}{
		{
			name: "fully configured",
			cfg: ConfigFile{
				Auth: ConfigAuth{
					APIKey: &ConfigAPIKey{
						AuthnURL: "https://authn.nvidia.com/token",
						Key:      "nvapi-xxx",
					},
				},
			},
			want: true,
		},
		{
			name: "missing key",
			cfg: ConfigFile{
				Auth: ConfigAuth{
					APIKey: &ConfigAPIKey{AuthnURL: "https://authn.nvidia.com/token"},
				},
			},
			want: false,
		},
		{
			name: "nil api key",
			cfg:  ConfigFile{},
			want: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := HasAPIKeyConfig(&tt.cfg)
			if got != tt.want {
				t.Errorf("HasAPIKeyConfig() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestSaveConfigToPath(t *testing.T) {
	t.Run("preserves unknown keys", func(t *testing.T) {
		dir := t.TempDir()
		path := filepath.Join(dir, "config.yaml")
		initial := "api:\n  base: http://localhost\ncustom_key: my-value\n"
		require.NoError(t, os.WriteFile(path, []byte(initial), 0600))

		cfg, err := LoadConfigFromPath(path)
		require.NoError(t, err)
		cfg.API.Org = "test-org"
		require.NoError(t, SaveConfigToPath(cfg, path))

		data, err := os.ReadFile(path)
		require.NoError(t, err)
		content := string(data)
		require.Contains(t, content, "custom_key")
		require.Contains(t, content, "test-org")
		require.Contains(t, content, "http://localhost")
	})

	t.Run("rejects malformed existing config", func(t *testing.T) {
		path := filepath.Join(t.TempDir(), "config.yaml")
		require.NoError(t, os.WriteFile(path, []byte("api: ["), 0600))

		err := SaveConfigToPath(&ConfigFile{}, path)
		require.ErrorContains(t, err, "parsing existing config")
	})
}

func TestLoadConfigFromPath_NotFound(t *testing.T) {
	cfg, err := LoadConfigFromPath("/nonexistent/path/config.yaml")
	if err != nil {
		t.Fatalf("expected no error for missing file, got %v", err)
	}
	if cfg.API.Base != "" || cfg.API.Org != "" {
		t.Errorf("expected empty config, got %+v", cfg)
	}
}

func TestLoadConfigFromPath_Roundtrip(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "config.yaml")

	original := &ConfigFile{
		API: ConfigAPI{
			Base: "http://localhost:8388",
			Org:  "test-org",
			Name: "nico",
		},
		Auth: ConfigAuth{
			OIDC: &ConfigOIDC{
				TokenURL:         "http://localhost:8080/realms/nico-dev/protocol/openid-connect/token",
				ClientID:         "nico-api",
				ClientSecret:     "secret",
				ClientAuthMethod: "client_secret_basic",
				Scopes:           []string{"carbide", "offline_access"},
				TokenParameters:  map[string]string{"audience": "nico"},
				Token:            "eyJhbG...",
				RefreshToken:     "refresh...",
				ExpiresAt:        "2026-01-01T00:00:00Z",
			},
		},
	}

	if err := SaveConfigToPath(original, path); err != nil {
		t.Fatal(err)
	}

	loaded, err := LoadConfigFromPath(path)
	if err != nil {
		t.Fatal(err)
	}

	if loaded.API.Base != original.API.Base {
		t.Errorf("API.Base = %q, want %q", loaded.API.Base, original.API.Base)
	}
	if loaded.API.Org != original.API.Org {
		t.Errorf("API.Org = %q, want %q", loaded.API.Org, original.API.Org)
	}
	if loaded.Auth.OIDC == nil {
		t.Fatal("Auth.OIDC is nil after roundtrip")
	}
	if loaded.Auth.OIDC.Token != original.Auth.OIDC.Token {
		t.Errorf("Auth.OIDC.Token = %q, want %q", loaded.Auth.OIDC.Token, original.Auth.OIDC.Token)
	}
	if loaded.Auth.OIDC.ClientSecret != original.Auth.OIDC.ClientSecret {
		t.Errorf("Auth.OIDC.ClientSecret = %q, want %q", loaded.Auth.OIDC.ClientSecret, original.Auth.OIDC.ClientSecret)
	}
	require.Equal(t, original.Auth.OIDC.ClientAuthMethod, loaded.Auth.OIDC.ClientAuthMethod)
	require.Equal(t, original.Auth.OIDC.Scopes, loaded.Auth.OIDC.Scopes)
	require.Equal(t, original.Auth.OIDC.TokenParameters, loaded.Auth.OIDC.TokenParameters)
}

func contains(s, substr string) bool {
	return len(s) >= len(substr) && (s == substr || len(s) > 0 && containsHelper(s, substr))
}

func containsHelper(s, substr string) bool {
	for i := 0; i <= len(s)-len(substr); i++ {
		if s[i:i+len(substr)] == substr {
			return true
		}
	}
	return false
}
