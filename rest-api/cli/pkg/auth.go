// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cli

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"strings"
	"time"

	cli "github.com/urfave/cli/v2"
	"golang.org/x/term"
)

// TokenResponse is the OAuth2 token endpoint response.
type TokenResponse struct {
	AccessToken  string `json:"access_token"`
	RefreshToken string `json:"refresh_token"`
	ExpiresIn    int    `json:"expires_in"`
	TokenType    string `json:"token_type"`
}

// LoginCommand returns the 'login' CLI command.
func LoginCommand() *cli.Command {
	return &cli.Command{
		Name:  "login",
		Usage: "Authenticate and save the token",
		Flags: []cli.Flag{
			&cli.StringFlag{
				Name:    "username",
				Usage:   "Username for OIDC password grant",
				EnvVars: []string{"NICO_OIDC_USERNAME"},
			},
			&cli.StringFlag{
				Name:    "password",
				Usage:   "Password for OIDC password grant (prompted if not provided)",
				EnvVars: []string{"NICO_OIDC_PASSWORD"},
			},
			&cli.StringFlag{
				Name:    "client-secret",
				Usage:   "Client secret (required for confidential OIDC clients)",
				EnvVars: []string{"NICO_CLIENT_SECRET"},
			},
			&cli.StringFlag{
				Name:    "api-key",
				Usage:   "NGC API key for token exchange",
				EnvVars: []string{"NICO_API_KEY"},
			},
			&cli.StringFlag{
				Name:    "authn-url",
				Usage:   "NGC authentication URL for API key exchange",
				EnvVars: []string{"NICO_AUTHN_URL"},
			},
		},
		Action: func(c *cli.Context) error {
			cfg, err := LoadConfig()
			if err != nil {
				return fmt.Errorf("loading config: %w", err)
			}
			ApplyEnvOverrides(cfg)

			tokenCommand := c.String("token-command")
			if tokenCommand != "" {
				if _, err := LoginWithTokenCommand(cfg, ConfigPath(), tokenCommand); err != nil {
					return err
				}
				fmt.Fprintf(os.Stderr, "Login successful (auth script). Token saved to %s\n", ConfigPath())
				return nil
			}

			explicitAPIKey := hasExplicitAPIKeyLoginFlags(c)
			apiKey := c.String("api-key")
			if apiKey == "" && explicitAPIKey && cfg.Auth.APIKey != nil {
				apiKey = cfg.Auth.APIKey.Key
			}
			if explicitAPIKey {
				if apiKey == "" {
					return fmt.Errorf("api-key must be provided when using API key auth")
				}
				authnURL := c.String("authn-url")
				if authnURL == "" && cfg.Auth.APIKey != nil && cfg.Auth.APIKey.AuthnURL != "" {
					authnURL = cfg.Auth.APIKey.AuthnURL
				}
				if authnURL == "" && !isNGCBearerAPIKey(apiKey) {
					return fmt.Errorf("authn-url must be provided when using --api-key (legacy NGC keys); not required for nvapi- prefixed keys")
				}
				return loginWithAPIKey(cfg, authnURL, apiKey)
			}

			if hasExplicitOIDCLoginFlags(c) {
				return loginWithOIDCCmd(c, cfg)
			}

			if HasTokenCommandConfig(cfg) {
				if _, err := LoginWithTokenCommand(cfg, ConfigPath(), cfg.Auth.TokenCommand); err != nil {
					return err
				}
				fmt.Fprintf(os.Stderr, "Login successful (auth script). Token saved to %s\n", ConfigPath())
				return nil
			}
			if HasOIDCConfig(cfg) {
				_, err := LoginWithOIDCConfig(cfg, ConfigPath())
				if err != nil {
					return err
				}
				fmt.Fprintf(os.Stderr, "Login successful. Token saved to %s\n", ConfigPath())
				return nil
			}
			if cfg.Auth.APIKey != nil && cfg.Auth.APIKey.Key != "" {
				if cfg.Auth.APIKey.AuthnURL == "" && !isNGCBearerAPIKey(cfg.Auth.APIKey.Key) {
					return fmt.Errorf("auth.api_key.authn_url is required in config (legacy NGC keys); not required for nvapi- prefixed keys")
				}
				return loginWithAPIKey(cfg, cfg.Auth.APIKey.AuthnURL, cfg.Auth.APIKey.Key)
			}
			return loginWithOIDCCmd(c, cfg)
		},
	}
}

// isNGCBearerAPIKey reports whether an NGC API key is a Personal or Service key
// that already serves as a bearer token. These keys are prefixed with "nvapi-"
// and do not need to be exchanged via the authn URL. Legacy NGC API keys
// (without the prefix) must still be exchanged for a bearer token.
func isNGCBearerAPIKey(apiKey string) bool {
	return strings.HasPrefix(apiKey, "nvapi-")
}

func hasExplicitOIDCLoginFlags(c *cli.Context) bool {
	for _, name := range []string{"token-url", "keycloak-url", "keycloak-realm", "client-id", "client-secret", "username", "password"} {
		if cliFlagExplicitlySet(c, name) {
			return true
		}
	}
	return false
}

func hasExplicitAPIKeyLoginFlags(c *cli.Context) bool {
	return cliFlagExplicitlySet(c, "api-key") || cliFlagExplicitlySet(c, "authn-url")
}

func cliFlagExplicitlySet(c *cli.Context, name string) bool {
	for _, flagName := range c.FlagNames() {
		if flagName == name {
			return commandLineContainsFlag(name)
		}
	}
	return false
}

func commandLineContainsFlag(name string) bool {
	longFlag := "--" + name
	for _, arg := range os.Args[1:] {
		if arg == longFlag || strings.HasPrefix(arg, longFlag+"=") {
			return true
		}
	}
	return false
}

// InitCommand returns the 'init' CLI command that generates a sample config.
func InitCommand() *cli.Command {
	return &cli.Command{
		Name:  "init",
		Usage: "Generate a sample config file at ~/.nico/config.yaml",
		Action: func(c *cli.Context) error {
			path := ConfigPath()
			if _, err := os.Stat(path); err == nil {
				return fmt.Errorf("config file already exists: %s", path)
			}
			if err := os.MkdirAll(ConfigDir(), 0700); err != nil {
				return fmt.Errorf("creating config directory: %w", err)
			}
			if err := os.WriteFile(path, []byte(SampleConfig), 0600); err != nil {
				return fmt.Errorf("writing config: %w", err)
			}
			fmt.Fprintf(os.Stderr, "Config written to %s\n", path)
			return nil
		},
	}
}

// LoginFromConfig performs a login using the configured auth method and returns
// the current bearer token.
func LoginFromConfig(cfg *ConfigFile, configPath string) (string, error) {
	if HasTokenCommandConfig(cfg) {
		return LoginWithTokenCommand(cfg, configPath, cfg.Auth.TokenCommand)
	}
	if HasOIDCConfig(cfg) {
		return LoginWithOIDCConfig(cfg, configPath)
	}
	if HasAPIKeyConfig(cfg) {
		return ExchangeAPIKey(cfg, configPath)
	}
	return "", fmt.Errorf("no auth method configured")
}

// LoginWithTokenCommand runs a shell command that prints a bearer token,
// stores it in config, and returns it.
func LoginWithTokenCommand(cfg *ConfigFile, configPath, tokenCommand string) (string, error) {
	token, err := ExecuteTokenCommand(tokenCommand)
	if err != nil {
		return "", err
	}
	cfg.Auth.TokenCommand = tokenCommand
	cfg.Auth.Token = token
	if err := SaveConfigToPath(cfg, configPath); err != nil {
		return "", fmt.Errorf("saving config: %w", err)
	}
	return token, nil
}

// ExecuteTokenCommand runs a shell command that prints a bearer token.
func ExecuteTokenCommand(tokenCommand string) (string, error) {
	token, err := ResolveToken("", tokenCommand)
	if err != nil {
		return "", err
	}
	if token == "" {
		return "", fmt.Errorf("auth script did not return a token")
	}
	return token, nil
}

// LoginWithOIDCConfig performs a non-interactive OIDC login from config.
func LoginWithOIDCConfig(cfg *ConfigFile, configPath string) (string, error) {
	if !HasOIDCConfig(cfg) {
		return "", fmt.Errorf("no OIDC auth configured")
	}
	oidc := cfg.Auth.OIDC

	var tokenResp *TokenResponse
	var err error
	if oidc.RefreshToken != "" {
		tokenResp, err = refreshTokenGrant(oidc.TokenURL, oidc.ClientID, oidc.ClientSecret, oidc.RefreshToken)
	}
	if tokenResp == nil && oidc.Username == "" && oidc.ClientSecret != "" {
		tokenResp, err = clientCredentialsGrant(oidc)
	}
	if tokenResp == nil && oidc.Username != "" && oidc.Password != "" {
		tokenResp, err = passwordGrant(oidc.TokenURL, oidc.ClientID, oidc.ClientSecret, oidc.Username, oidc.Password)
	}
	if tokenResp == nil {
		if err != nil {
			// Nothing is defaulted on this path, every value came from config, so the
			// hint carries only the endpoint that was contacted.
			return "", fmt.Errorf("OIDC login failed: %w%s", err, loginFailureHint(oidc.TokenURL, nil))
		}
		return "", fmt.Errorf("OIDC login requires auth.oidc.refresh_token, client credentials, or username/password in config")
	}

	if err := saveOIDCToken(oidc, tokenResp); err != nil {
		return "", err
	}
	if err := SaveConfigToPath(cfg, configPath); err != nil {
		return "", fmt.Errorf("saving config: %w", err)
	}
	return tokenResp.AccessToken, nil
}

// ExchangeAPIKey exchanges an NGC API key for a bearer token, updates the config,
// and saves it. Returns the new token. NGC Personal/Service API keys (those
// prefixed with "nvapi-") are bearer tokens themselves and are returned as-is
// without contacting the authn endpoint.
func ExchangeAPIKey(cfg *ConfigFile, configPath string) (string, error) {
	if cfg.Auth.APIKey == nil || cfg.Auth.APIKey.Key == "" {
		return "", fmt.Errorf("no NGC API key configured")
	}
	if isNGCBearerAPIKey(cfg.Auth.APIKey.Key) {
		cfg.Auth.APIKey.Token = cfg.Auth.APIKey.Key
		if saveErr := SaveConfigToPath(cfg, configPath); saveErr != nil {
			fmt.Fprintf(os.Stderr, "Warning: could not save token: %v\n", saveErr)
		}
		return cfg.Auth.APIKey.Key, nil
	}
	authnURL := cfg.Auth.APIKey.AuthnURL
	if authnURL == "" {
		return "", fmt.Errorf("auth.api_key.authn_url is required in config")
	}
	req, err := http.NewRequest("GET", authnURL, nil)
	if err != nil {
		return "", fmt.Errorf("building request: %w", err)
	}
	req.Header.Set("Authorization", "ApiKey "+cfg.Auth.APIKey.Key)
	req.Header.Set("Accept", "application/json")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return "", fmt.Errorf("requesting token from NGC: %w", err)
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return "", fmt.Errorf("reading response: %w", err)
	}
	if resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("NGC token exchange failed (HTTP %d): %s", resp.StatusCode, string(body))
	}

	token := extractNGCToken(body)
	if token == "" {
		return "", fmt.Errorf("NGC response did not contain a token")
	}

	cfg.Auth.APIKey.Token = token
	if saveErr := SaveConfigToPath(cfg, configPath); saveErr != nil {
		fmt.Fprintf(os.Stderr, "Warning: could not save token: %v\n", saveErr)
	}
	return token, nil
}

func loginWithAPIKey(cfg *ConfigFile, authnURL, apiKey string) error {
	if isNGCBearerAPIKey(apiKey) {
		if cfg.Auth.APIKey == nil {
			cfg.Auth.APIKey = &ConfigAPIKey{}
		}
		cfg.Auth.APIKey.Key = apiKey
		cfg.Auth.APIKey.Token = apiKey
		if err := SaveConfig(cfg); err != nil {
			return fmt.Errorf("saving config: %w", err)
		}
		fmt.Fprintf(os.Stderr, "Login successful (NGC API key). Token saved to %s\n", ConfigPath())
		return nil
	}
	req, err := http.NewRequest("GET", authnURL, nil)
	if err != nil {
		return fmt.Errorf("building request: %w", err)
	}
	req.Header.Set("Authorization", "ApiKey "+apiKey)
	req.Header.Set("Accept", "application/json")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return fmt.Errorf("requesting token from NGC: %w", err)
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return fmt.Errorf("reading response: %w", err)
	}
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("NGC token exchange failed (HTTP %d): %s", resp.StatusCode, string(body))
	}

	token := extractNGCToken(body)
	if token == "" {
		return fmt.Errorf("NGC response did not contain a token")
	}

	if cfg.Auth.APIKey == nil {
		cfg.Auth.APIKey = &ConfigAPIKey{}
	}
	cfg.Auth.APIKey.Token = token
	if err := SaveConfig(cfg); err != nil {
		return fmt.Errorf("saving config: %w", err)
	}
	fmt.Fprintf(os.Stderr, "Login successful (NGC API key). Token saved to %s\n", ConfigPath())
	return nil
}

func extractNGCToken(body []byte) string {
	var resp struct {
		Token       string `json:"token"`
		AccessToken string `json:"access_token"`
	}
	if json.Unmarshal(body, &resp) != nil {
		return ""
	}
	if resp.Token != "" {
		return resp.Token
	}
	return resp.AccessToken
}

// resolveOIDCRealm returns the Keycloak realm used to build the token endpoint from
// --keycloak-url, and whether it came from the flag's built-in default rather than from
// the command line or config. An explicit flag wins over config, matching client-id.
func resolveOIDCRealm(c *cli.Context, cfg *ConfigFile) (realm string, fromDefault bool) {
	if cliFlagExplicitlySet(c, "keycloak-realm") {
		return c.String("keycloak-realm"), false
	}
	if cfg.Auth.OIDC != nil && cfg.Auth.OIDC.Realm != "" {
		return cfg.Auth.OIDC.Realm, false
	}
	return c.String("keycloak-realm"), true
}

// resolveOIDCClientID returns the OAuth client ID, and whether it came from the flag's
// built-in default rather than from the command line or config.
func resolveOIDCClientID(c *cli.Context, cfg *ConfigFile) (clientID string, fromDefault bool) {
	if cliFlagExplicitlySet(c, "client-id") {
		return c.String("client-id"), false
	}
	if cfg.Auth.OIDC != nil && cfg.Auth.OIDC.ClientID != "" {
		return cfg.Auth.OIDC.ClientID, false
	}
	return c.String("client-id"), true
}

// loginFailureHint names the token endpoint that was actually contacted, plus any values
// that came from a built-in default. A realm or client that does not exist in the target
// Keycloak fails with a 404 or invalid_client that identifies neither, so without this
// the built-in defaults are invisible in the error.
func loginFailureHint(tokenURL string, defaulted []string) string {
	hint := "\n  token endpoint: " + tokenURL
	if len(defaulted) > 0 {
		hint += "\n  using built-in default " + strings.Join(defaulted, " and ")
		hint += "\n  pass the flag explicitly, or set auth.oidc in " + ConfigPath()
	}
	return hint
}

func loginWithOIDCCmd(c *cli.Context, cfg *ConfigFile) error {
	tokenURL := c.String("token-url")
	if tokenURL == "" && cfg.Auth.OIDC != nil {
		tokenURL = cfg.Auth.OIDC.TokenURL
	}

	// Values that fell back to a built-in default, recorded so a failed login can name
	// them. Only populated where the value was actually used: a realm supplied through
	// --token-url never goes through resolveOIDCRealm.
	var defaulted []string
	var resolvedRealm string

	if tokenURL == "" {
		keycloakURL := c.String("keycloak-url")
		if keycloakURL != "" {
			realm, realmFromDefault := resolveOIDCRealm(c, cfg)
			if realmFromDefault {
				defaulted = append(defaulted, "--keycloak-realm="+realm)
			}
			resolvedRealm = realm
			tokenURL = fmt.Sprintf("%s/realms/%s/protocol/openid-connect/token",
				strings.TrimRight(keycloakURL, "/"), realm)
		}
	}
	if tokenURL == "" {
		return fmt.Errorf("--token-url or --keycloak-url is required (or set auth.oidc.token_url in config)")
	}

	clientID, clientIDFromDefault := resolveOIDCClientID(c, cfg)
	if clientIDFromDefault {
		defaulted = append(defaulted, "--client-id="+clientID)
	}

	clientSecret := c.String("client-secret")
	if clientSecret == "" && cfg.Auth.OIDC != nil {
		clientSecret = cfg.Auth.OIDC.ClientSecret
	}

	username := c.String("username")
	if username == "" && cfg.Auth.OIDC != nil {
		username = cfg.Auth.OIDC.Username
	}

	password := c.String("password")
	if password == "" && cfg.Auth.OIDC != nil {
		password = cfg.Auth.OIDC.Password
	}

	var tokenResp *TokenResponse
	var err error

	if username == "" && clientSecret != "" {
		requestOIDC := ConfigOIDC{}
		if cfg.Auth.OIDC != nil {
			requestOIDC = *cfg.Auth.OIDC
		}
		requestOIDC.TokenURL = tokenURL
		requestOIDC.ClientID = clientID
		requestOIDC.ClientSecret = clientSecret
		tokenResp, err = clientCredentialsGrant(&requestOIDC)
	} else {
		if username == "" {
			fmt.Print("Username: ")
			if _, scanErr := fmt.Scanln(&username); scanErr != nil {
				return fmt.Errorf("reading username: %w", scanErr)
			}
		}
		if password == "" {
			fmt.Print("Password: ")
			pw, pwErr := term.ReadPassword(int(os.Stdin.Fd()))
			fmt.Println()
			if pwErr != nil {
				return fmt.Errorf("reading password: %w", pwErr)
			}
			password = string(pw)
		}
		tokenResp, err = passwordGrant(tokenURL, clientID, clientSecret, username, password)
	}
	if err != nil {
		return fmt.Errorf("%w%s", err, loginFailureHint(tokenURL, defaulted))
	}

	if cfg.Auth.OIDC == nil {
		cfg.Auth.OIDC = &ConfigOIDC{}
	}
	if err := saveOIDCToken(cfg.Auth.OIDC, tokenResp); err != nil {
		return err
	}
	cfg.Auth.OIDC.TokenURL = tokenURL
	cfg.Auth.OIDC.ClientID = clientID
	cfg.Auth.OIDC.ClientSecret = clientSecret
	// Only persist a realm that was used to build tokenURL; a realm is meaningless
	// against a token endpoint supplied directly.
	if resolvedRealm != "" {
		cfg.Auth.OIDC.Realm = resolvedRealm
	}

	if err := SaveConfig(cfg); err != nil {
		return fmt.Errorf("saving config: %w", err)
	}
	fmt.Fprintf(os.Stderr, "Login successful. Token saved to %s\n", ConfigPath())
	return nil
}

func passwordGrant(tokenURL, clientID, clientSecret, username, password string) (*TokenResponse, error) {
	data := url.Values{
		"grant_type": {"password"},
		"client_id":  {clientID},
		"username":   {username},
		"password":   {password},
		"scope":      {"openid"},
	}
	if clientSecret != "" {
		data.Set("client_secret", clientSecret)
	}
	return postToken(tokenURL, data)
}

func clientCredentialsGrant(oidc *ConfigOIDC) (*TokenResponse, error) {
	const (
		clientSecretPost  = "client_secret_post"
		clientSecretBasic = "client_secret_basic"
	)
	reserved := map[string]struct{}{
		"grant_type": {}, "client_id": {}, "client_secret": {}, "scope": {},
		"username": {}, "password": {}, "refresh_token": {},
		"client_assertion": {}, "client_assertion_type": {},
	}
	data := url.Values{
		"grant_type": {"client_credentials"},
	}
	scopes := oidc.Scopes
	if len(scopes) == 0 {
		scopes = []string{"openid"}
	}
	data.Set("scope", strings.Join(scopes, " "))
	for name, value := range oidc.TokenParameters {
		if _, blocked := reserved[name]; blocked {
			return nil, fmt.Errorf("reserved token parameter %q cannot be configured", name)
		}
		data.Set(name, value)
	}

	method := oidc.ClientAuthMethod
	if method == "" {
		method = clientSecretPost
	}
	switch method {
	case clientSecretPost:
		data.Set("client_id", oidc.ClientID)
		data.Set("client_secret", oidc.ClientSecret)
		return postToken(oidc.TokenURL, data)
	case clientSecretBasic:
		return postTokenWithBasicAuth(oidc.TokenURL, data, oidc.ClientID, oidc.ClientSecret)
	default:
		return nil, fmt.Errorf("unsupported client_auth_method %q", method)
	}
}

func postTokenWithBasicAuth(tokenURL string, data url.Values, clientID, clientSecret string) (*TokenResponse, error) {
	req, err := http.NewRequest(http.MethodPost, tokenURL, strings.NewReader(data.Encode()))
	if err != nil {
		return nil, fmt.Errorf("token request: %w", err)
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.SetBasicAuth(url.QueryEscape(clientID), url.QueryEscape(clientSecret))
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("token request: %w", err)
	}
	return parseTokenResponse(resp)
}

func refreshTokenGrant(tokenURL, clientID, clientSecret, refreshToken string) (*TokenResponse, error) {
	data := url.Values{
		"grant_type":    {"refresh_token"},
		"client_id":     {clientID},
		"refresh_token": {refreshToken},
	}
	if clientSecret != "" {
		data.Set("client_secret", clientSecret)
	}
	return postToken(tokenURL, data)
}

func postToken(tokenURL string, data url.Values) (*TokenResponse, error) {
	resp, err := http.PostForm(tokenURL, data)
	if err != nil {
		return nil, fmt.Errorf("token request: %w", err)
	}
	return parseTokenResponse(resp)
}

func parseTokenResponse(resp *http.Response) (*TokenResponse, error) {
	defer resp.Body.Close()

	if resp.StatusCode != 200 {
		body, _ := io.ReadAll(resp.Body)
		var errBody struct {
			Error       string `json:"error"`
			Description string `json:"error_description"`
		}
		if json.Unmarshal(body, &errBody) == nil && errBody.Description != "" {
			return nil, fmt.Errorf("authentication failed: %s", errBody.Description)
		}
		if len(body) > 0 {
			return nil, fmt.Errorf("authentication failed (%s): %s", resp.Status, string(body))
		}
		return nil, fmt.Errorf("authentication failed: %s", resp.Status)
	}

	var tokenResp TokenResponse
	if err := json.NewDecoder(resp.Body).Decode(&tokenResp); err != nil {
		return nil, fmt.Errorf("decoding token response: %w", err)
	}
	return &tokenResp, nil
}

// AutoRefreshToken attempts to refresh the OIDC token if it is near expiry.
func AutoRefreshToken(cfg *ConfigFile) (string, error) {
	return AutoRefreshTokenToPath(cfg, ConfigPath())
}

// AutoRefreshTokenToPath attempts to refresh the OIDC token if it is near expiry
// and saves refreshed tokens to configPath.
func AutoRefreshTokenToPath(cfg *ConfigFile, configPath string) (string, error) {
	if cfg.Auth.OIDC == nil {
		return GetAuthToken(cfg), nil
	}

	oidc := cfg.Auth.OIDC
	if oidc.Token == "" {
		return "", nil
	}
	if oidc.ExpiresAt == "" {
		return oidc.Token, nil
	}

	expiresAt, err := time.Parse(time.RFC3339, oidc.ExpiresAt)
	if err != nil {
		return oidc.Token, nil
	}

	if time.Now().Before(expiresAt.Add(-30 * time.Second)) {
		return oidc.Token, nil
	}

	if oidc.RefreshToken == "" || oidc.ClientID == "" || oidc.TokenURL == "" {
		return oidc.Token, nil
	}

	tokenResp, err := refreshTokenGrant(oidc.TokenURL, oidc.ClientID, oidc.ClientSecret, oidc.RefreshToken)
	if err != nil {
		return oidc.Token, nil
	}

	if err := saveOIDCToken(oidc, tokenResp); err != nil {
		return oidc.Token, err
	}
	if err := SaveConfigToPath(cfg, configPath); err != nil {
		fmt.Fprintf(os.Stderr, "Warning: token refreshed but could not save config: %v\n", err)
	}

	return oidc.Token, nil
}

func saveOIDCToken(oidc *ConfigOIDC, tokenResp *TokenResponse) error {
	if tokenResp.AccessToken == "" {
		return fmt.Errorf("OIDC token response did not contain an access token")
	}
	oidc.Token = tokenResp.AccessToken
	if tokenResp.RefreshToken != "" {
		oidc.RefreshToken = tokenResp.RefreshToken
	}
	if tokenResp.ExpiresIn > 0 {
		oidc.ExpiresAt = time.Now().Add(time.Duration(tokenResp.ExpiresIn) * time.Second).Format(time.RFC3339)
	}
	return nil
}
