// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cli

import (
	"fmt"
	"io"
	"os"

	cli "github.com/urfave/cli/v2"
)

// binaryName is the public name of the CLI binary as users see it on PATH and
// in --help output. It is centralized here so renames stay consistent across
// the cli.App.Name, generated UsageText, and error messages that reference
// the binary by name. Shell-completion script templates intentionally do not
// use this constant; they embed the name in identifiers like
// `_nicocli_complete` that would not benefit from string interpolation.
const binaryName = "nicocli"

// NewApp builds a cli.App from the embedded OpenAPI spec data.
func NewApp(specData []byte) (*cli.App, error) {
	spec, err := ParseSpec(specData)
	if err != nil {
		return nil, fmt.Errorf("parsing embedded spec: %w", err)
	}

	defaultBaseURL := ""
	if len(spec.Servers) > 0 {
		defaultBaseURL = spec.Servers[0].URL
	}

	commands := BuildCommands(spec)
	commands = addSiteBootstrapCommand(commands, spec)
	commands = append(commands, LoginCommand())
	commands = append(commands, InitCommand())
	commands = append(commands, completionCommand())

	app := &cli.App{
		Name:                 binaryName,
		Usage:                spec.Info.Title,
		Version:              spec.Info.Version,
		EnableBashCompletion: true,
		Flags: []cli.Flag{
			&cli.StringFlag{
				Name:    "config",
				Usage:   "Path to config file",
				EnvVars: []string{"NICO_CONFIG"},
			},
			&cli.StringFlag{
				Name:    "base-url",
				Usage:   "API base URL",
				EnvVars: []string{"NICO_BASE_URL"},
				Value:   defaultBaseURL,
			},
			&cli.StringFlag{
				Name:    "org",
				Usage:   "Organization name",
				EnvVars: []string{"NICO_ORG"},
			},
			&cli.StringFlag{
				Name:    "api-name",
				Usage:   "API path segment used in /v2/org/<org>/<name>/... routes",
				EnvVars: []string{"NICO_API_NAME"},
				Value:   "nico",
			},
			&cli.StringFlag{
				Name:    "token",
				Usage:   "API bearer token",
				EnvVars: []string{"NICO_TOKEN"},
			},
			&cli.StringFlag{
				Name:    "token-command",
				Aliases: []string{"auth-script"},
				Usage:   "Shell command/script that prints a bearer token",
				EnvVars: []string{"NICO_TOKEN_COMMAND", "NICO_AUTH_SCRIPT"},
			},
			&cli.BoolFlag{
				Name:  "debug",
				Usage: "Enable debug logging (HTTP request/response details and NICO_* env vars, with recognized secrets redacted)",
			},
			&cli.StringFlag{
				Name:    "token-url",
				Usage:   "OIDC token endpoint URL for login and token refresh",
				EnvVars: []string{"NICO_TOKEN_URL"},
			},
			&cli.StringFlag{
				Name:    "keycloak-url",
				Usage:   "Keycloak base URL (constructs token-url if --token-url is not set)",
				EnvVars: []string{"NICO_KEYCLOAK_URL"},
			},
			&cli.StringFlag{
				Name:    "keycloak-realm",
				Usage:   "Keycloak realm (used with --keycloak-url)",
				EnvVars: []string{"NICO_KEYCLOAK_REALM"},
				Value:   "nico-dev",
			},
			&cli.StringFlag{
				Name:    "client-id",
				Usage:   "OAuth client ID",
				EnvVars: []string{"NICO_CLIENT_ID"},
				Value:   "nico-api",
			},
		},
		Commands: commands,
		Before: func(c *cli.Context) error {
			if cfg := c.String("config"); cfg != "" {
				SetConfigPath(cfg)
			}
			if c.Bool("debug") {
				printEnvOverridesForDebug(os.Stderr)
			}
			return nil
		},
	}

	return app, nil
}

// printEnvOverridesForDebug writes the list of NICO_* env vars currently
// set in the process environment to w, prefixed with "[debug] env:". The
// listing reflects what nicocli will pull from the environment when it
// applies overrides on top of the loaded config; flag-only env vars
// (NICO_KEYCLOAK_URL, NICO_KEYCLOAK_REALM, NICO_CONFIG) are included so
// users can see every NICO_* knob the CLI reads. Sensitive values are masked
// because debug output is commonly captured in terminal transcripts and logs.
func printEnvOverridesForDebug(w io.Writer) {
	overrides := EnvOverridesFromEnvironment()
	if len(overrides) == 0 {
		fmt.Fprintln(w, "[debug] env: no NICO_* environment variables set")
		return
	}
	fmt.Fprintf(w, "[debug] env: %d NICO_* variable(s) in use\n", len(overrides))
	for _, line := range splitLines(FormatEnvOverrides(overrides, true)) {
		if line == "" {
			continue
		}
		fmt.Fprintf(w, "[debug] env: %s\n", line)
	}
}

// splitLines splits s on '\n' without keeping a trailing empty element,
// so callers iterating it don't have to special-case the final newline.
func splitLines(s string) []string {
	var out []string
	start := 0
	for i := 0; i < len(s); i++ {
		if s[i] == '\n' {
			out = append(out, s[start:i])
			start = i + 1
		}
	}
	if start < len(s) {
		out = append(out, s[start:])
	}
	return out
}

func completionCommand() *cli.Command {
	return &cli.Command{
		Name:  "completion",
		Usage: "Output shell completion script",
		Subcommands: []*cli.Command{
			{
				Name:  "bash",
				Usage: "Output bash completion script",
				Action: func(c *cli.Context) error {
					fmt.Print(bashCompletion)
					return nil
				},
			},
			{
				Name:  "zsh",
				Usage: "Output zsh completion script",
				Action: func(c *cli.Context) error {
					fmt.Print(zshCompletion)
					return nil
				},
			},
			{
				Name:  "fish",
				Usage: "Output fish completion script",
				Action: func(c *cli.Context) error {
					fmt.Print(fishCompletion)
					return nil
				},
			},
		},
	}
}

const bashCompletion = `# bash completion for nicocli
# Add to ~/.bashrc:  eval "$(nicocli completion bash)"
_nicocli_complete() {
    local cur opts
    COMPREPLY=()
    cur="${COMP_WORDS[COMP_CWORD]}"
    opts=$(${COMP_WORDS[0]} --generate-bash-completion "${COMP_WORDS[@]:1:$COMP_CWORD}")
    COMPREPLY=($(compgen -W "${opts}" -- "${cur}"))
    return 0
}
complete -o default -F _nicocli_complete nicocli
`

const zshCompletion = `# zsh completion for nicocli
# Add to ~/.zshrc:  eval "$(nicocli completion zsh)"
_nicocli_complete() {
    local -a opts
    opts=(${(f)"$(${words[1]} --generate-bash-completion ${words:1:$CURRENT-1})"})
    _describe 'nicocli' opts
}
compdef _nicocli_complete nicocli
`

const fishCompletion = `# fish completion for nicocli
# Add to ~/.config/fish/completions/nicocli.fish or run:
#   nicocli completion fish > ~/.config/fish/completions/nicocli.fish
complete -c nicocli -f -a '(nicocli --generate-bash-completion (commandline -cop))'
`
