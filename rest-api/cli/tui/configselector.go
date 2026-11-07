// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"golang.org/x/term"

	cli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
)

// ChooseConfigFile scans ~/.nico for config*.yaml files and shows an interactive
// selector if multiple configs exist. Returns the chosen path, or empty string
// if only one config exists (use default) or no terminal is available.
func ChooseConfigFile(explicitPath string) (string, error) {
	if explicitPath != "" {
		return explicitPath, nil
	}
	if !term.IsTerminal(int(os.Stdin.Fd())) || !term.IsTerminal(int(os.Stdout.Fd())) {
		return "", nil
	}

	home, err := os.UserHomeDir()
	if err != nil {
		return "", nil
	}

	configDir := filepath.Join(home, ".nico")
	entries, err := os.ReadDir(configDir)
	if err != nil {
		if os.IsNotExist(err) {
			return "", nil
		}
		return "", fmt.Errorf("reading config directory: %w", err)
	}

	var candidates []string
	for _, entry := range entries {
		if entry.IsDir() {
			continue
		}
		name := entry.Name()
		if !strings.HasPrefix(name, "config") {
			continue
		}
		if !strings.HasSuffix(name, ".yaml") && !strings.HasSuffix(name, ".yml") {
			continue
		}
		candidates = append(candidates, filepath.Join(configDir, name))
	}

	if len(candidates) <= 1 {
		return "", nil
	}

	sort.Slice(candidates, func(i, j int) bool {
		left := filepath.Base(candidates[i])
		right := filepath.Base(candidates[j])
		leftDefault := left == "config.yaml" || left == "config.yml"
		rightDefault := right == "config.yaml" || right == "config.yml"
		if leftDefault != rightDefault {
			return leftDefault
		}
		return left < right
	})

	items := make([]SelectItem, len(candidates))
	for i, path := range candidates {
		display := path
		if strings.HasPrefix(path, home+string(os.PathSeparator)) {
			display = "~/" + strings.TrimPrefix(path, home+string(os.PathSeparator))
		}
		items[i] = SelectItem{Label: display, ID: path}
	}

	fmt.Println()
	selected, err := Select("Choose config for this session", items)
	if err != nil {
		return "", err
	}
	fmt.Printf("Using config: %s\n\n", selected.Label)
	return selected.ID, nil
}

// RunTUI is the entry point for cli tui. It handles config selection,
// authentication, and starts the REPL.
func RunTUI(explicitConfig string) error {
	configPath, err := ChooseConfigFile(explicitConfig)
	if err != nil {
		return fmt.Errorf("choosing config: %w", err)
	}

	var cfg *cli.ConfigFile
	if configPath != "" {
		cfg, err = cli.LoadConfigFromPath(configPath)
	} else {
		cfg, err = cli.LoadConfig()
		configPath = cli.ConfigPath()
	}
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	cli.ApplyEnvOverrides(cfg)

	org := cfg.API.Org
	if org == "" {
		return fmt.Errorf("api.org is required in config %s", configPath)
	}

	baseURL := cfg.API.Base
	apiName := cfg.API.Name
	if apiName == "" {
		apiName = "nico"
	}

	token, refreshErr := cli.AutoRefreshTokenToPath(cfg, configPath)
	if refreshErr != nil {
		return fmt.Errorf("refreshing auth token: %w", refreshErr)
	}
	if token == "" {
		token = cli.GetAuthToken(cfg)
	}
	if token == "" && (cli.HasTokenCommandConfig(cfg) || cli.HasOIDCConfig(cfg) || cli.HasAPIKeyConfig(cfg)) {
		token, err = cli.LoginFromConfig(cfg, configPath)
		if err != nil {
			return fmt.Errorf("logging in from config: %w", err)
		}
	}

	client := cli.NewClient(baseURL, org, token, nil, false)
	client.APIName = apiName

	session := NewSession(client, org, configPath)
	session.Token = token

	if cli.HasTokenCommandConfig(cfg) || cli.HasOIDCConfig(cfg) || cli.HasAPIKeyConfig(cfg) {
		loginFn := func() (string, error) {
			return cli.LoginFromConfig(cfg, configPath)
		}
		session.LoginFn = loginFn
		client.TokenRefresh = func() (string, error) {
			token, err := loginFn()
			if err == nil && token != "" {
				session.RefreshClient(token)
			}
			return token, err
		}
		client.AuthRetryNotify = func(event cli.AuthRetryEvent) {
			switch event.Action {
			case cli.AuthRetryActionLogin:
				fmt.Fprintf(os.Stderr, "%s API returned %d; running configured login (%d/%d).\n",
					Yellow("Auth:"), event.StatusCode, event.Attempt, event.MaxAttempts)
			case cli.AuthRetryActionRetry:
				fmt.Fprintf(os.Stderr, "%s Retrying API request with refreshed token (%d/%d).\n",
					Yellow("Auth:"), event.Attempt, event.MaxAttempts)
			case cli.AuthRetryActionSkip:
				fmt.Fprintf(os.Stderr, "%s API returned %d for %s; automatic retry skipped for non-idempotent request. Run %s and retry the command.\n",
					Yellow("Auth:"), event.StatusCode, event.Method, Bold("login"))
			}
		}
	}

	if token == "" {
		fmt.Fprintf(os.Stderr, "%s No auth token found. Type %s to authenticate.\n\n",
			Yellow("Warning:"), Bold("login"))
	}

	return RunREPL(session)
}
