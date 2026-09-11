// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cli

import (
	"flag"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"testing"
	"time"

	"github.com/stretchr/testify/require"
	cli "github.com/urfave/cli/v2"
)

func TestLoginWithOIDCConfig(t *testing.T) {
	tests := []struct {
		name         string
		oidc         ConfigOIDC
		checkRequest func(*testing.T, *http.Request)
		// rejectStatus, when non-zero, makes the token endpoint fail so the case can
		// assert what a failed configured login reports back.
		rejectStatus      int
		wantErr           string
		wantEndpointInErr bool
		wantRequests      int
		wantSavedToken    bool
	}{
		{
			name: "custom client credentials request",
			oidc: ConfigOIDC{
				ClientID:         "client:id",
				ClientSecret:     "client+secret%",
				Scopes:           []string{"carbide", "offline_access"},
				TokenParameters:  map[string]string{"audience": "nico"},
				ClientAuthMethod: "client_secret_basic",
			},
			checkRequest: func(t *testing.T, r *http.Request) {
				require.NoError(t, r.ParseForm())
				require.Equal(t, "client_credentials", r.Form.Get("grant_type"))
				require.Equal(t, "carbide offline_access", r.Form.Get("scope"))
				require.Equal(t, "nico", r.Form.Get("audience"))
				require.Empty(t, r.Form.Get("client_id"))
				require.Empty(t, r.Form.Get("client_secret"))
				clientID, clientSecret, ok := r.BasicAuth()
				require.True(t, ok)
				decodedClientID, err := url.QueryUnescape(clientID)
				require.NoError(t, err)
				decodedClientSecret, err := url.QueryUnescape(clientSecret)
				require.NoError(t, err)
				require.Equal(t, "client:id", decodedClientID)
				require.Equal(t, "client+secret%", decodedClientSecret)
			},
			wantRequests:   1,
			wantSavedToken: true,
		},
		{
			name: "default client credentials request",
			oidc: ConfigOIDC{ClientID: "client-id", ClientSecret: "client-secret"},
			checkRequest: func(t *testing.T, r *http.Request) {
				require.NoError(t, r.ParseForm())
				require.Equal(t, "client_credentials", r.Form.Get("grant_type"))
				require.Equal(t, "openid", r.Form.Get("scope"))
				require.Equal(t, "client-id", r.Form.Get("client_id"))
				require.Equal(t, "client-secret", r.Form.Get("client_secret"))
				require.Empty(t, r.Header.Get("Authorization"))
			},
			wantRequests: 1,
		},
		{
			name: "reserved token parameter",
			oidc: ConfigOIDC{
				ClientID:        "client-id",
				ClientSecret:    "client-secret",
				TokenParameters: map[string]string{"client_secret": "replacement"},
			},
			wantErr: "reserved token parameter",
		},
		{
			name:              "failed grant names the token endpoint",
			oidc:              ConfigOIDC{ClientID: "client-id", ClientSecret: "client-secret"},
			rejectStatus:      http.StatusNotFound,
			wantErr:           "token endpoint: ",
			wantEndpointInErr: true,
			wantRequests:      1,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			requests := 0
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				requests++
				if tt.checkRequest != nil {
					tt.checkRequest(t, r)
				}
				if tt.rejectStatus != 0 {
					w.WriteHeader(tt.rejectStatus)
					return
				}
				w.Header().Set("Content-Type", "application/json")
				_, _ = w.Write([]byte(`{"access_token":"new-token","expires_in":3600}`))
			}))
			defer server.Close()

			tt.oidc.TokenURL = server.URL
			cfg := &ConfigFile{Auth: ConfigAuth{OIDC: &tt.oidc}}
			configPath := filepath.Join(t.TempDir(), "config.yaml")
			token, err := LoginWithOIDCConfig(cfg, configPath)
			if tt.wantErr != "" {
				require.ErrorContains(t, err, tt.wantErr)
				if tt.wantEndpointInErr {
					require.ErrorContains(t, err, server.URL)
				}
			} else {
				require.NoError(t, err)
				require.Equal(t, "new-token", token)
			}
			require.Equal(t, tt.wantRequests, requests)
			if tt.wantSavedToken {
				loaded, err := LoadConfigFromPath(configPath)
				require.NoError(t, err)
				require.Equal(t, "new-token", loaded.Auth.OIDC.Token)
			}
		})
	}
}

func TestLoginWithOIDCCmd(t *testing.T) {
	tests := []struct {
		name         string
		clientIDArgs []string
		wantClientID string
		// rejectStatus, when non-zero, makes the token endpoint fail so the case can
		// assert what a failed login reports back to the user.
		rejectStatus int
		wantErr      string
	}{
		{
			name:         "preserves configured client ID",
			wantClientID: "client-id",
		},
		{
			name:         "uses explicit client ID",
			clientIDArgs: []string{"--client-id", "override-id"},
			wantClientID: "override-id",
		},
		{
			name:         "failed login names the token endpoint",
			wantClientID: "client-id",
			rejectStatus: http.StatusNotFound,
			wantErr:      "token endpoint: ",
		},
	}

	// `--keycloak-realm` and `--client-id` are registered on the app, not on the login
	// command, so resolveOIDCRealm only sees an explicit flag when it is passed in
	// global position. Exercised here rather than against a hand-built flag set.
	t.Run("global keycloak-realm flag builds the token endpoint", func(t *testing.T) {
		var gotPath string
		server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			gotPath = r.URL.Path
			w.WriteHeader(http.StatusNotFound)
		}))
		defer server.Close()

		configPath := filepath.Join(t.TempDir(), "config.yaml")
		cfg := &ConfigFile{Auth: ConfigAuth{OIDC: &ConfigOIDC{Realm: "from-config"}}}
		require.NoError(t, SaveConfigToPath(cfg, configPath))
		SetConfigPath(configPath)
		defer SetConfigPath("")

		app, err := NewApp([]byte(`{"openapi":"3.0.0","info":{"title":"test","version":"test"},"paths":{}}`))
		require.NoError(t, err)
		withArgs(t, "nicocli", "--keycloak-url", server.URL, "--keycloak-realm", "from-flag",
			"login", "--client-secret", "secret")
		require.Error(t, app.Run(os.Args))
		require.Equal(t, "/realms/from-flag/protocol/openid-connect/token", gotPath)
	})

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				require.NoError(t, r.ParseForm())
				require.Equal(t, "carbide", r.Form.Get("scope"))
				require.Equal(t, "nico", r.Form.Get("audience"))
				clientID, clientSecret, ok := r.BasicAuth()
				require.True(t, ok)
				require.Equal(t, tt.wantClientID, clientID)
				require.Equal(t, "client-secret", clientSecret)
				if tt.rejectStatus != 0 {
					w.WriteHeader(tt.rejectStatus)
					return
				}
				w.Header().Set("Content-Type", "application/json")
				_, _ = w.Write([]byte(`{"access_token":"new-token","expires_in":3600}`))
			}))
			defer server.Close()

			configPath := filepath.Join(t.TempDir(), "config.yaml")
			cfg := &ConfigFile{Auth: ConfigAuth{OIDC: &ConfigOIDC{
				TokenURL:         "https://auth.example.invalid/token",
				ClientID:         "client-id",
				ClientSecret:     "client-secret",
				Scopes:           []string{"carbide"},
				TokenParameters:  map[string]string{"audience": "nico"},
				ClientAuthMethod: "client_secret_basic",
			}}}
			require.NoError(t, SaveConfigToPath(cfg, configPath))
			SetConfigPath(configPath)
			defer SetConfigPath("")
			app, err := NewApp([]byte(`{"openapi":"3.0.0","info":{"title":"test","version":"test"},"paths":{}}`))
			require.NoError(t, err)
			args := append([]string{"nicocli", "--token-url", server.URL}, tt.clientIDArgs...)
			args = append(args, "login")
			withArgs(t, args...)

			runErr := app.Run(os.Args)
			if tt.wantErr != "" {
				require.Error(t, runErr)
				require.Contains(t, runErr.Error(), tt.wantErr)
				require.Contains(t, runErr.Error(), server.URL)
				return
			}
			require.NoError(t, runErr)
		})
	}
}

func TestResolveOIDCRealm(t *testing.T) {
	tests := []struct {
		name            string
		args            []string
		configRealm     string
		wantRealm       string
		wantFromDefault bool
	}{
		{
			name:            "falls back to the flag default",
			wantRealm:       "nico-dev",
			wantFromDefault: true,
		},
		{
			name:        "configured realm beats the flag default",
			configRealm: "nico",
			wantRealm:   "nico",
		},
		{
			name:        "explicit flag beats the configured realm",
			args:        []string{"--keycloak-realm", "nico-prod"},
			configRealm: "nico",
			wantRealm:   "nico-prod",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			withArgs(t, append([]string{"nicocli"}, tt.args...)...)
			c := newRealmFlagContext(t, tt.args)
			cfg := &ConfigFile{Auth: ConfigAuth{OIDC: &ConfigOIDC{Realm: tt.configRealm}}}

			realm, fromDefault := resolveOIDCRealm(c, cfg)
			require.Equal(t, tt.wantRealm, realm)
			require.Equal(t, tt.wantFromDefault, fromDefault)
		})
	}
}

func TestResolveOIDCClientID(t *testing.T) {
	tests := []struct {
		name            string
		args            []string
		configClientID  string
		wantClientID    string
		wantFromDefault bool
	}{
		{
			name:            "falls back to the flag default",
			wantClientID:    "nico-api",
			wantFromDefault: true,
		},
		{
			name:           "configured client beats the flag default",
			configClientID: "nico-rest",
			wantClientID:   "nico-rest",
		},
		{
			name:           "explicit flag beats the configured client",
			args:           []string{"--client-id", "override-id"},
			configClientID: "nico-rest",
			wantClientID:   "override-id",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			withArgs(t, append([]string{"nicocli"}, tt.args...)...)
			c := newClientIDFlagContext(t, tt.args)
			cfg := &ConfigFile{Auth: ConfigAuth{OIDC: &ConfigOIDC{ClientID: tt.configClientID}}}

			clientID, fromDefault := resolveOIDCClientID(c, cfg)
			require.Equal(t, tt.wantClientID, clientID)
			require.Equal(t, tt.wantFromDefault, fromDefault)
		})
	}
}

func TestLoginFailureHint(t *testing.T) {
	tests := []struct {
		name        string
		defaulted   []string
		wantContain []string
		wantAbsent  string
	}{
		{
			name:        "reports only the endpoint when nothing was defaulted",
			wantContain: []string{"token endpoint: https://kc.example/token"},
			wantAbsent:  "built-in default",
		},
		{
			name:        "names a single default",
			defaulted:   []string{"--keycloak-realm=nico-dev"},
			wantContain: []string{"built-in default --keycloak-realm=nico-dev", "auth.oidc"},
		},
		{
			name:        "joins multiple defaults",
			defaulted:   []string{"--keycloak-realm=nico-dev", "--client-id=nico-api"},
			wantContain: []string{"--keycloak-realm=nico-dev and --client-id=nico-api"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			hint := loginFailureHint("https://kc.example/token", tt.defaulted)
			for _, want := range tt.wantContain {
				require.Contains(t, hint, want)
			}
			if tt.wantAbsent != "" {
				require.NotContains(t, hint, tt.wantAbsent)
			}
		})
	}
}

// newRealmFlagContext builds a cli.Context carrying only the keycloak-realm flag, with
// the same default the login command declares.
func newRealmFlagContext(t *testing.T, args []string) *cli.Context {
	t.Helper()
	set := flag.NewFlagSet("test", flag.ContinueOnError)
	set.String("keycloak-realm", "nico-dev", "")
	require.NoError(t, set.Parse(args))
	return cli.NewContext(nil, set, nil)
}

// newClientIDFlagContext builds a cli.Context carrying only the client-id flag, with the
// same default the login command declares.
func newClientIDFlagContext(t *testing.T, args []string) *cli.Context {
	t.Helper()
	set := flag.NewFlagSet("test", flag.ContinueOnError)
	set.String("client-id", "nico-api", "")
	require.NoError(t, set.Parse(args))
	return cli.NewContext(nil, set, nil)
}

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

func TestLoginCommandReturnsConfigReadError(t *testing.T) {
	configPath := t.TempDir()
	SetConfigPath(configPath)
	defer SetConfigPath("")

	ctx := cli.NewContext(cli.NewApp(), flag.NewFlagSet("login", flag.ContinueOnError), nil)
	err := LoginCommand().Action(ctx)
	require.Error(t, err)
	require.Contains(t, err.Error(), "loading config:")
	require.Contains(t, err.Error(), configPath)
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

func withArgs(t *testing.T, args ...string) {
	t.Helper()
	oldArgs := os.Args
	os.Args = append([]string(nil), args...)
	t.Cleanup(func() {
		os.Args = oldArgs
	})
}
