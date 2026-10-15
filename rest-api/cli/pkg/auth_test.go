// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cli

import (
	"flag"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strconv"
	"testing"
	"time"

	"github.com/stretchr/testify/require"
	cli "github.com/urfave/cli/v2"
)

func TestExtractNGCToken(t *testing.T) {
	tests := []struct {
		name string
		body string
		want string
	}{
		{
			name: "token field",
			body: `{"token": "abc123"}`,
			want: "abc123",
		},
		{
			name: "access_token field",
			body: `{"access_token": "xyz789"}`,
			want: "xyz789",
		},
		{
			name: "token takes precedence over access_token",
			body: `{"token": "primary", "access_token": "secondary"}`,
			want: "primary",
		},
		{
			name: "empty response",
			body: `{}`,
			want: "",
		},
		{
			name: "invalid json",
			body: `not json`,
			want: "",
		},
		{
			name: "empty token values",
			body: `{"token": "", "access_token": ""}`,
			want: "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := extractNGCToken([]byte(tt.body))
			require.Equal(t, tt.want, got)
		})
	}
}

func TestLoginWithTokenCommandSavesTokenAndCommand(t *testing.T) {
	dir := t.TempDir()
	configPath := filepath.Join(dir, "config.yaml")
	markerPath := filepath.Join(dir, "script-ran")
	scriptPath := filepath.Join(dir, "token.sh")
	script := "#!/bin/sh\n" +
		"printf ran > " + strconv.Quote(markerPath) + "\n" +
		"printf script-token\n"
	require.NoError(t, os.WriteFile(scriptPath, []byte(script), 0600))
	cfg := &ConfigFile{}
	tokenCommand := "sh " + strconv.Quote(scriptPath)

	token, err := LoginWithTokenCommand(cfg, configPath, tokenCommand)
	require.NoError(t, err)
	require.Equal(t, "script-token", token)
	require.FileExists(t, markerPath)

	loaded, err := LoadConfigFromPath(configPath)
	require.NoError(t, err)
	require.Equal(t, "script-token", loaded.Auth.Token)
	require.Equal(t, tokenCommand, loaded.Auth.TokenCommand)
}

func TestLoginWithTokenCommandRejectsEmptyOutput(t *testing.T) {
	cfg := &ConfigFile{}
	_, err := LoginWithTokenCommand(cfg, filepath.Join(t.TempDir(), "config.yaml"), "printf ''")
	require.Error(t, err)
}

func TestAutoRefreshTokenToPathSavesSelectedConfig(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		require.NoError(t, r.ParseForm())
		require.Equal(t, "refresh_token", r.Form.Get("grant_type"))
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"access_token":"new-token","refresh_token":"new-refresh","expires_in":3600}`))
	}))
	defer server.Close()

	dir := t.TempDir()
	defaultPath := filepath.Join(dir, "default.yaml")
	selectedPath := filepath.Join(dir, "selected.yaml")
	SetConfigPath(defaultPath)
	defer SetConfigPath("")

	cfg := &ConfigFile{
		Auth: ConfigAuth{
			OIDC: &ConfigOIDC{
				TokenURL:     server.URL,
				ClientID:     "client-id",
				Token:        "old-token",
				RefreshToken: "old-refresh",
				ExpiresAt:    time.Now().Add(-time.Hour).Format(time.RFC3339),
			},
		},
	}

	token, err := AutoRefreshTokenToPath(cfg, selectedPath)
	require.NoError(t, err)
	require.Equal(t, "new-token", token)

	selected, err := LoadConfigFromPath(selectedPath)
	require.NoError(t, err)
	require.Equal(t, "new-token", selected.Auth.OIDC.Token)
	_, err = os.Stat(defaultPath)
	require.True(t, os.IsNotExist(err), "default config should not be written, stat err=%v", err)
}

func TestSaveOIDCTokenPreservesExistingRefreshTokenWhenOmitted(t *testing.T) {
	oidc := &ConfigOIDC{RefreshToken: "existing-refresh", ExpiresAt: "2026-01-01T00:00:00Z"}
	require.NoError(t, saveOIDCToken(oidc, &TokenResponse{AccessToken: "new-token", ExpiresIn: 3600}))
	require.Equal(t, "new-token", oidc.Token)
	require.Equal(t, "existing-refresh", oidc.RefreshToken)
	require.NotEqual(t, "2026-01-01T00:00:00Z", oidc.ExpiresAt)
}

func TestSaveOIDCTokenPreservesExpiresAtWhenExpiresInMissing(t *testing.T) {
	oidc := &ConfigOIDC{Token: "old-token", RefreshToken: "old-refresh", ExpiresAt: "2026-01-01T00:00:00Z"}
	require.NoError(t, saveOIDCToken(oidc, &TokenResponse{AccessToken: "new-token", RefreshToken: "new-refresh"}))
	require.Equal(t, "new-token", oidc.Token)
	require.Equal(t, "new-refresh", oidc.RefreshToken)
	require.Equal(t, "2026-01-01T00:00:00Z", oidc.ExpiresAt)
}

func TestSaveOIDCTokenErrorsWhenAccessTokenMissing(t *testing.T) {
	oidc := &ConfigOIDC{
		Token:        "existing-token",
		RefreshToken: "existing-refresh",
		ExpiresAt:    "2026-01-01T00:00:00Z",
	}

	err := saveOIDCToken(oidc, &TokenResponse{RefreshToken: "new-refresh", ExpiresIn: 3600})

	require.Error(t, err)
	require.Equal(t, "existing-token", oidc.Token)
	require.Equal(t, "existing-refresh", oidc.RefreshToken)
	require.Equal(t, "2026-01-01T00:00:00Z", oidc.ExpiresAt)
}

func TestLoginCommandExplicitAPIKeyWinsOverOIDCFlags(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		require.Equal(t, "ApiKey explicit-key", r.Header.Get("Authorization"))
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"token":"api-token"}`))
	}))
	defer server.Close()

	configPath := filepath.Join(t.TempDir(), "config.yaml")
	cfg := &ConfigFile{
		Auth: ConfigAuth{
			OIDC: &ConfigOIDC{
				TokenURL: "https://oidc.example.invalid/token",
				ClientID: "client-id",
			},
			APIKey: &ConfigAPIKey{
				AuthnURL: server.URL,
			},
		},
	}
	require.NoError(t, SaveConfigToPath(cfg, configPath))
	SetConfigPath(configPath)
	defer SetConfigPath("")

	flags := flag.NewFlagSet("login", flag.ContinueOnError)
	for _, name := range []string{"api-key", "authn-url", "token-url", "keycloak-url", "keycloak-realm", "client-id", "client-secret", "username", "password", "token-command"} {
		flags.String(name, "", "")
	}
	require.NoError(t, flags.Set("api-key", "explicit-key"))
	require.NoError(t, flags.Set("token-url", "https://oidc.example.invalid/token"))
	withArgs(t, "carbidecli", "login", "--api-key", "explicit-key", "--token-url", "https://oidc.example.invalid/token")

	ctx := cli.NewContext(cli.NewApp(), flags, nil)
	require.NoError(t, LoginCommand().Action(ctx))

	loaded, err := LoadConfigFromPath(configPath)
	require.NoError(t, err)
	require.Equal(t, "api-token", loaded.Auth.APIKey.Token)
}

func TestLoginCommandConfiguredOIDCUsesRefreshToken(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		require.NoError(t, r.ParseForm())
		require.Equal(t, "refresh_token", r.Form.Get("grant_type"))
		require.Equal(t, "stored-refresh", r.Form.Get("refresh_token"))
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"access_token":"refreshed-token","refresh_token":"new-refresh","expires_in":3600}`))
	}))
	defer server.Close()

	configPath := filepath.Join(t.TempDir(), "config.yaml")
	cfg := &ConfigFile{
		Auth: ConfigAuth{
			OIDC: &ConfigOIDC{
				TokenURL:     server.URL,
				ClientID:     "client-id",
				RefreshToken: "stored-refresh",
			},
		},
	}
	require.NoError(t, SaveConfigToPath(cfg, configPath))
	SetConfigPath(configPath)
	defer SetConfigPath("")

	flags := flag.NewFlagSet("login", flag.ContinueOnError)
	for _, name := range []string{"api-key", "authn-url", "token-url", "keycloak-url", "keycloak-realm", "client-id", "client-secret", "username", "password", "token-command"} {
		flags.String(name, "", "")
	}

	ctx := cli.NewContext(cli.NewApp(), flags, nil)
	require.NoError(t, LoginCommand().Action(ctx))

	loaded, err := LoadConfigFromPath(configPath)
	require.NoError(t, err)
	require.Equal(t, "refreshed-token", loaded.Auth.OIDC.Token)
	require.Equal(t, "new-refresh", loaded.Auth.OIDC.RefreshToken)
}

func TestLoginCommandExplicitAPIKeyRequiresAuthnURL(t *testing.T) {
	configPath := filepath.Join(t.TempDir(), "config.yaml")
	SetConfigPath(configPath)
	defer SetConfigPath("")

	flags := flag.NewFlagSet("login", flag.ContinueOnError)
	for _, name := range []string{"api-key", "authn-url", "token-url", "keycloak-url", "keycloak-realm", "client-id", "client-secret", "username", "password", "token-command"} {
		flags.String(name, "", "")
	}
	require.NoError(t, flags.Set("api-key", "explicit-key"))
	withArgs(t, "carbidecli", "login", "--api-key", "explicit-key")

	ctx := cli.NewContext(cli.NewApp(), flags, nil)
	err := LoginCommand().Action(ctx)
	require.Error(t, err)
	require.Contains(t, err.Error(), "authn-url")
}

func TestLoginCommandExplicitAPIKeyModeRequiresKey(t *testing.T) {
	configPath := filepath.Join(t.TempDir(), "config.yaml")
	SetConfigPath(configPath)
	defer SetConfigPath("")

	flags := flag.NewFlagSet("login", flag.ContinueOnError)
	for _, name := range []string{"api-key", "authn-url", "token-url", "keycloak-url", "keycloak-realm", "client-id", "client-secret", "username", "password", "token-command"} {
		flags.String(name, "", "")
	}
	require.NoError(t, flags.Set("authn-url", "https://auth.example.invalid/token"))
	withArgs(t, "carbidecli", "login", "--authn-url", "https://auth.example.invalid/token")

	ctx := cli.NewContext(cli.NewApp(), flags, nil)
	err := LoginCommand().Action(ctx)
	require.Error(t, err)
	require.Contains(t, err.Error(), "api-key")
}

func TestEnvAuthFlagsDoNotSelectExplicitAPIKeyMode(t *testing.T) {
	configPath := filepath.Join(t.TempDir(), "config.yaml")
	cfg := &ConfigFile{Auth: ConfigAuth{TokenCommand: "printf script-token"}}
	require.NoError(t, SaveConfigToPath(cfg, configPath))
	SetConfigPath(configPath)
	defer SetConfigPath("")

	t.Setenv("CARBIDE_AUTHN_URL", "https://auth.example.invalid/token")

	app, err := NewApp([]byte(`{"openapi":"3.0.0","info":{"title":"test","version":"test"},"paths":{}}`))
	require.NoError(t, err)
	require.NoError(t, app.Run([]string{"carbidecli", "login"}))

	loaded, err := LoadConfigFromPath(configPath)
	require.NoError(t, err)
	require.Equal(t, "script-token", loaded.Auth.Token)
}

func TestLoginCommandConfiguredAPIKeyRequiresAuthnURL(t *testing.T) {
	configPath := filepath.Join(t.TempDir(), "config.yaml")
	cfg := &ConfigFile{
		Auth: ConfigAuth{
			APIKey: &ConfigAPIKey{Key: "configured-key"},
		},
	}
	require.NoError(t, SaveConfigToPath(cfg, configPath))
	SetConfigPath(configPath)
	defer SetConfigPath("")

	flags := flag.NewFlagSet("login", flag.ContinueOnError)
	for _, name := range []string{"api-key", "authn-url", "token-url", "keycloak-url", "keycloak-realm", "client-id", "client-secret", "username", "password", "token-command"} {
		flags.String(name, "", "")
	}

	ctx := cli.NewContext(cli.NewApp(), flags, nil)
	err := LoginCommand().Action(ctx)
	require.Error(t, err)
	require.Contains(t, err.Error(), "auth.api_key.authn_url")
}

func TestLoginCommandExplicitNvapiKeySkipsAuthnURL(t *testing.T) {
	configPath := filepath.Join(t.TempDir(), "config.yaml")
	SetConfigPath(configPath)
	defer SetConfigPath("")

	flags := flag.NewFlagSet("login", flag.ContinueOnError)
	for _, name := range []string{"api-key", "authn-url", "token-url", "keycloak-url", "keycloak-realm", "client-id", "client-secret", "username", "password", "token-command"} {
		flags.String(name, "", "")
	}
	require.NoError(t, flags.Set("api-key", "nvapi-explicit-key"))
	withArgs(t, "nicocli", "login", "--api-key", "nvapi-explicit-key")

	ctx := cli.NewContext(cli.NewApp(), flags, nil)
	require.NoError(t, LoginCommand().Action(ctx))

	loaded, err := LoadConfigFromPath(configPath)
	require.NoError(t, err)
	require.Equal(t, "nvapi-explicit-key", loaded.Auth.APIKey.Token)
	require.Equal(t, "nvapi-explicit-key", loaded.Auth.APIKey.Key)
}

func TestLoginCommandConfiguredNvapiKeySkipsAuthnURL(t *testing.T) {
	configPath := filepath.Join(t.TempDir(), "config.yaml")
	cfg := &ConfigFile{
		Auth: ConfigAuth{
			APIKey: &ConfigAPIKey{Key: "nvapi-configured-key"},
		},
	}
	require.NoError(t, SaveConfigToPath(cfg, configPath))
	SetConfigPath(configPath)
	defer SetConfigPath("")

	flags := flag.NewFlagSet("login", flag.ContinueOnError)
	for _, name := range []string{"api-key", "authn-url", "token-url", "keycloak-url", "keycloak-realm", "client-id", "client-secret", "username", "password", "token-command"} {
		flags.String(name, "", "")
	}

	ctx := cli.NewContext(cli.NewApp(), flags, nil)
	require.NoError(t, LoginCommand().Action(ctx))

	loaded, err := LoadConfigFromPath(configPath)
	require.NoError(t, err)
	require.Equal(t, "nvapi-configured-key", loaded.Auth.APIKey.Token)
}

func TestExchangeAPIKeyNvapiReturnsKeyDirectly(t *testing.T) {
	configPath := filepath.Join(t.TempDir(), "config.yaml")
	cfg := &ConfigFile{
		Auth: ConfigAuth{
			APIKey: &ConfigAPIKey{Key: "nvapi-bearer-key"},
		},
	}

	token, err := ExchangeAPIKey(cfg, configPath)
	require.NoError(t, err)
	require.Equal(t, "nvapi-bearer-key", token)
	require.Equal(t, "nvapi-bearer-key", cfg.Auth.APIKey.Token)

	loaded, err := LoadConfigFromPath(configPath)
	require.NoError(t, err)
	require.Equal(t, "nvapi-bearer-key", loaded.Auth.APIKey.Token)
}

func TestIsNGCBearerAPIKey(t *testing.T) {
	require.True(t, isNGCBearerAPIKey("nvapi-abc"))
	require.True(t, isNGCBearerAPIKey("nvapi-"))
	require.False(t, isNGCBearerAPIKey("legacy-key"))
	require.False(t, isNGCBearerAPIKey(""))
	require.False(t, isNGCBearerAPIKey("NVAPI-uppercase"))
}

// TestLoginWithOIDCCmd runs the real app, because --keycloak-url, --keycloak-realm, and
// --client-id are global flags that only reach the login command in global position.
func TestLoginWithOIDCCmd(t *testing.T) {
	type tokenRequest struct {
		path     string
		clientID string
	}
	tests := []struct {
		name           string
		configTokenURL string
		configClientID string
		clientIDArgs   []string
		wantClientID   string
	}{
		{
			// The configured endpoint is unreachable, so if it won, no request would arrive.
			name:           "explicit keycloak-url beats configured token_url",
			configTokenURL: "https://auth.example.invalid/realms/nico-dev/protocol/openid-connect/token",
			clientIDArgs:   []string{"--client-id", "flag-client"},
			wantClientID:   "flag-client",
		},
		{
			name:           "configured client_id is used when --client-id is not passed",
			configClientID: "config-client",
			wantClientID:   "config-client",
		},
		{
			name:           "explicit client-id beats configured client_id",
			configClientID: "config-client",
			clientIDArgs:   []string{"--client-id", "flag-client"},
			wantClientID:   "flag-client",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := make(chan tokenRequest, 1)
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				_ = r.ParseForm()
				got <- tokenRequest{path: r.URL.Path, clientID: r.Form.Get("client_id")}
				w.WriteHeader(http.StatusNotFound)
			}))
			defer server.Close()

			configPath := filepath.Join(t.TempDir(), "config.yaml")
			cfg := &ConfigFile{Auth: ConfigAuth{OIDC: &ConfigOIDC{
				TokenURL: tt.configTokenURL,
				ClientID: tt.configClientID,
			}}}
			require.NoError(t, SaveConfigToPath(cfg, configPath))
			SetConfigPath(configPath)
			defer SetConfigPath("")

			app, err := NewApp([]byte(`{"openapi":"3.0.0","info":{"title":"test","version":"test"},"paths":{}}`))
			require.NoError(t, err)
			args := append([]string{"nicocli", "--keycloak-url", server.URL, "--keycloak-realm", "nico"}, tt.clientIDArgs...)
			withArgs(t, append(args, "login", "--client-secret", "secret")...)
			require.Error(t, app.Run(os.Args))

			select {
			case req := <-got:
				require.Equal(t, "/realms/nico/protocol/openid-connect/token", req.path)
				require.Equal(t, tt.wantClientID, req.clientID)
			default:
				t.Fatal("login never reached the --keycloak-url token endpoint")
			}
		})
	}
}

func withArgs(t *testing.T, args ...string) {
	t.Helper()
	oldArgs := os.Args
	os.Args = append([]string(nil), args...)
	t.Cleanup(func() {
		os.Args = oldArgs
	})
}
