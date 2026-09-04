// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cli

import (
	"bytes"
	"flag"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/openapi"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	cli "github.com/urfave/cli/v2"
)

func TestToKebab(t *testing.T) {
	tests := []struct {
		input string
		want  string
	}{
		// Space-separated (tag names)
		{"Site", "site"},
		{"IP Block", "ip-block"},
		{"SSH Key Group", "ssh-key-group"},
		{"DPU Extension Service", "dpu-extension-service"},
		{"Infrastructure Provider", "infrastructure-provider"},
		{"NVLink Logical Partition", "nvlink-logical-partition"},

		// CamelCase (parameter and field names)
		{"siteId", "site-id"},
		{"pageNumber", "page-number"},
		{"pageSize", "page-size"},
		{"infrastructureProviderId", "infrastructure-provider-id"},
		{"networkSecurityGroupId", "network-security-group-id"},
		{"serialConsoleHostname", "serial-console-hostname"},

		// Acronym handling
		{"NVLinkLogicalPartition", "nvlink-logical-partition"},
		{"isNVLinkPartitionEnabled", "is-nvlink-partition-enabled"},
		{"dpuExtensionServiceId", "dpu-extension-service-id"},

		// Already lowercase
		{"site", "site"},
		{"org", "org"},

		// Single word uppercase
		{"ID", "id"},
		{"VPC", "vpc"},
		{"SKU", "sku"},
	}

	for _, tt := range tests {
		t.Run(tt.input, func(t *testing.T) {
			got := toKebab(tt.input)
			if got != tt.want {
				t.Errorf("toKebab(%q) = %q, want %q", tt.input, got, tt.want)
			}
		})
	}
}

func TestClientFromContextExplicitTokenCommandOverridesCachedConfigToken(t *testing.T) {
	configPath := filepath.Join(t.TempDir(), "config.yaml")
	cfg := &ConfigFile{
		API: ConfigAPI{
			Base: "http://localhost:8388",
			Org:  "test-org",
		},
		Auth: ConfigAuth{
			Token: "cached-token",
		},
	}
	require.NoError(t, SaveConfigToPath(cfg, configPath))
	SetConfigPath(configPath)
	defer SetConfigPath("")

	flags := flag.NewFlagSet("test", flag.ContinueOnError)
	flags.String("token", "", "")
	flags.String("token-command", "", "")
	flags.String("base-url", "", "")
	flags.String("org", "", "")
	flags.Bool("debug", false, "")
	require.NoError(t, flags.Set("token-command", "printf explicit-token"))

	ctx := cli.NewContext(cli.NewApp(), flags, nil)
	client, err := clientFromContext(ctx)
	require.NoError(t, err)
	assert.Equal(t, "explicit-token", client.Token)
}

func TestClientFromContextReturnsMalformedConfigError(t *testing.T) {
	configPath := filepath.Join(t.TempDir(), "config.yaml")
	require.NoError(t, os.WriteFile(configPath, []byte("auth:\n  token:missing-space\n  token_command: echo token\n"), 0600))
	SetConfigPath(configPath)
	defer SetConfigPath("")

	flags := flag.NewFlagSet("test", flag.ContinueOnError)
	flags.String("token", "", "")
	flags.String("token-command", "", "")
	flags.String("base-url", "", "")
	flags.String("org", "", "")
	flags.Bool("debug", false, "")

	ctx := cli.NewContext(cli.NewApp(), flags, nil)
	_, err := clientFromContext(ctx)
	require.Error(t, err)
	require.Contains(t, err.Error(), "loading config: parsing config "+configPath)
}

func TestOperationAction(t *testing.T) {
	tests := []struct {
		opID string
		want string
	}{
		{"get-all-site", "list"},
		{"get-all-instance", "list"},
		{"get-all-allocation-constraint", "list"},
		{"get-current-infrastructure-provider", "current"},
		{"get-current-tenant", "current"},
		{"get-current-service-account", "current"},
		{"create-site", "create"},
		{"create-allocation-constraint", "create"},
		{"update-site", "update"},
		{"delete-site", "delete"},
		{"get-site", "get"},
		{"get-allocation", "get"},
		{"get-site-status-history", "status-history"},
		{"get-instance-status-history", "status-history"},
		{"get-machine-status-history", "status-history"},
		// get-current-<resource> singletons resolve to `current`; their
		// -stats endpoints resolve to `stats` (distinct actions, no collision).
		// service-account has no -stats endpoint in the spec today, but the
		// suffix mapping is resource-independent, so it is asserted here too.
		{"get-current-infrastructure-provider-stats", "stats"},
		{"get-current-tenant-stats", "stats"},
		{"get-current-service-account-stats", "stats"},
		{"batch-create-instance", "batch-create"},
		{"batch-create-expected-machines", "batch-create"},
		{"batch-update-expected-machines", "batch-update"},
		{"get-metadata", "get"},
		{"get-user", "get"},
		{"update-identity-config", "update"},
		{"get-identity-config", "get"},
		{"delete-identity-config", "delete"},
		{"update-token-delegation", "update"},
		{"get-token-delegation", "get"},
		{"delete-token-delegation", "delete"},
		{"get-jwks", "get"},
		{"get-spiffe-jwks", "get"},
		{"get-openid-configuration", "get"},
		{"start-machine-validation", "start"},
	}

	for _, tt := range tests {
		t.Run(tt.opID, func(t *testing.T) {
			got := operationAction(tt.opID)
			if got != tt.want {
				t.Errorf("operationAction(%q) = %q, want %q", tt.opID, got, tt.want)
			}
		})
	}
}

func TestExtractResourceSuffix(t *testing.T) {
	tests := []struct {
		opID string
		want string
	}{
		{"get-all-site", "site"},
		{"create-site", "site"},
		{"get-site", "site"},
		{"delete-site", "site"},
		{"update-site", "site"},
		{"get-all-allocation-constraint", "allocation-constraint"},
		{"get-current-infrastructure-provider", "infrastructure-provider"},
		{"batch-create-expected-machines", "expected-machines"},
		{"batch-update-expected-machines", "expected-machines"},
		{"get-site-status-history", "site-status-history"},
		{"get-instance-status-history", "instance-status-history"},
		{"update-identity-config", "identity-config"},
		{"get-identity-config", "identity-config"},
		{"delete-identity-config", "identity-config"},
		{"update-token-delegation", "token-delegation"},
		{"get-token-delegation", "token-delegation"},
		{"delete-token-delegation", "token-delegation"},
		{"get-jwks", "jwks"},
		{"get-spiffe-jwks", "spiffe-jwks"},
		{"get-openid-configuration", "openid-configuration"},
		{"start-machine-validation", "machine-validation"},
	}

	for _, tt := range tests {
		t.Run(tt.opID, func(t *testing.T) {
			got := extractResourceSuffix(tt.opID)
			if got != tt.want {
				t.Errorf("extractResourceSuffix(%q) = %q, want %q", tt.opID, got, tt.want)
			}
		})
	}
}

func TestSubResourceName(t *testing.T) {
	tests := []struct {
		suffix  string
		primary string
		want    string
	}{
		// Exact match → primary
		{"site", "site", ""},
		{"allocation", "allocation", ""},
		{"instance", "instance", ""},

		// Plural match → primary
		{"expected-machines", "expected-machine", ""},

		// Primary as prefix → sub-resource
		{"allocation-constraint", "allocation", "constraint"},
		{"dpu-extension-service-version", "dpu-extension-service", "version"},
		{"instance-type-machine-association", "instance-type", "machine-association"},

		// Action modifiers → primary (not sub-resource)
		{"site-status-history", "site", ""},
		{"instance-status-history", "instance", ""},
		{"infrastructure-provider-stats", "infrastructure-provider", ""},

		// Primary as suffix → sub-resource
		{"derived-ipblock", "ipblock", "derived"},

		// No overlap → sub-resource (full suffix)
		{"interface", "instance", "interface"},
		{"infiniband-interface", "instance", "infiniband-interface"},
		{"nvlink-interface", "nvlink-logical-partition", "nvlink-interface"},
	}

	for _, tt := range tests {
		name := tt.suffix + "_primary_" + tt.primary
		t.Run(name, func(t *testing.T) {
			got := subResourceName(tt.suffix, tt.primary)
			if got != tt.want {
				t.Errorf("subResourceName(%q, %q) = %q, want %q", tt.suffix, tt.primary, got, tt.want)
			}
		})
	}
}

func TestDetectPrimaryResource(t *testing.T) {
	tests := []struct {
		name  string
		opIDs []string
		want  string
	}{
		{
			name: "site is primary",
			opIDs: []string{
				"get-all-site", "create-site", "get-site", "update-site", "delete-site",
				"get-site-status-history",
			},
			want: "site",
		},
		{
			name: "allocation wins over allocation-constraint by shorter length on tie",
			opIDs: []string{
				"get-all-allocation", "create-allocation", "get-allocation", "update-allocation", "delete-allocation",
				"get-all-allocation-constraint", "create-allocation-constraint", "get-allocation-constraint", "update-allocation-constraint", "delete-allocation-constraint",
			},
			want: "allocation",
		},
		{
			name: "instance wins with more operations",
			opIDs: []string{
				"get-all-instance", "create-instance", "get-instance", "update-instance", "delete-instance",
				"batch-create-instance", "get-instance-status-history",
				"get-all-interface",
				"get-all-infiniband-interface",
			},
			want: "instance",
		},
		{
			name: "expected-machine with plural batch ops",
			opIDs: []string{
				"create-expected-machine", "get-all-expected-machine", "get-expected-machine",
				"update-expected-machine", "delete-expected-machine",
				"batch-create-expected-machines", "batch-update-expected-machines",
			},
			want: "expected-machine",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ops := make([]resolvedOp, len(tt.opIDs))
			for i, opID := range tt.opIDs {
				ops[i] = resolvedOp{
					op: &Operation{OperationID: opID},
				}
			}
			got := detectPrimaryResource(ops)
			if got != tt.want {
				t.Errorf("detectPrimaryResource() = %q, want %q", got, tt.want)
			}
		})
	}
}

func TestCoerceValue(t *testing.T) {
	tests := []struct {
		value      string
		schemaType SchemaType
		want       interface{}
		wantErr    bool
	}{
		// Integers
		{"42", "integer", 42, false},
		{"0", "integer", 0, false},
		{"-1", "integer", -1, false},
		{"abc", "integer", nil, true},

		// Booleans
		{"true", "boolean", true, false},
		{"false", "boolean", false, false},
		{"1", "boolean", true, false},
		{"0", "boolean", false, false},
		{"yes", "boolean", nil, true},

		// Numbers (float)
		{"3.14", "number", 3.14, false},
		{"0", "number", float64(0), false},
		{"abc", "number", nil, true},

		// Strings (passthrough)
		{"hello", "string", "hello", false},
		{"", "string", "", false},
		{"123", "string", "123", false},
	}

	for _, tt := range tests {
		name := string(tt.schemaType) + "_" + tt.value
		t.Run(name, func(t *testing.T) {
			got, err := coerceValue(tt.value, tt.schemaType)
			if (err != nil) != tt.wantErr {
				t.Errorf("coerceValue(%q, %q) error = %v, wantErr %v", tt.value, tt.schemaType, err, tt.wantErr)
				return
			}
			if !tt.wantErr && got != tt.want {
				t.Errorf("coerceValue(%q, %q) = %v (%T), want %v (%T)", tt.value, tt.schemaType, got, got, tt.want, tt.want)
			}
		})
	}
}

func TestIsListAction(t *testing.T) {
	tests := []struct {
		action string
		want   bool
	}{
		{"list", true},
		{"list-interfaces", true},
		{"list-infiniband-interfaces", true},
		{"get", false},
		{"create", false},
		{"delete", false},
		{"listing", false},
	}

	for _, tt := range tests {
		t.Run(tt.action, func(t *testing.T) {
			got := isListAction(tt.action)
			if got != tt.want {
				t.Errorf("isListAction(%q) = %v, want %v", tt.action, got, tt.want)
			}
		})
	}
}

// TestBuildCommands_NoDuplicateFlags walks every command produced from the
// embedded OpenAPI spec and asserts that no single command declares the same
// flag name twice. Duplicate names cause urfave/cli to panic at
// flag.(*FlagSet).Var ("flag redefined") when the command's flag set is built,
// which is exactly how the dpu-extension-service create bug surfaced.
func TestBuildCommands_NoDuplicateFlags(t *testing.T) {
	spec, err := ParseSpec(openapi.Spec)
	require.NoError(t, err, "ParseSpec failed")

	cmds := BuildCommands(spec)
	require.NotEmpty(t, cmds, "BuildCommands returned no commands")

	var visit func(path string, children []*cli.Command)
	visit = func(path string, children []*cli.Command) {
		for _, c := range children {
			full := path + " " + c.Name
			seen := make(map[string]bool)
			for _, f := range c.Flags {
				name := f.Names()[0]
				assert.Falsef(t, seen[name],
					"command %q declares duplicate flag %q (would panic at runtime)", full, name)
				seen[name] = true
			}
			if len(c.Subcommands) > 0 {
				visit(full, c.Subcommands)
			}
		}
	}
	visit("nicocli", cmds)
}

func TestGeneratedCommandInfos_ContainsConciseAliases(t *testing.T) {
	spec, err := ParseSpec(openapi.Spec)
	require.NoError(t, err)

	operations := make(map[string]string)
	for _, info := range GeneratedCommandInfos(spec) {
		operations[info.Name] = info.OperationID
	}

	assert.Equal(t, "machine-power-control-machine", operations["machine power"])
	assert.Equal(t, "get-all-dpu-machines", operations["dpu-machine list"])
	assert.Equal(t, "get-dpu-machine", operations["dpu-machine get"])
	assert.Equal(t,
		"machine-power-control-machine",
		operations["machine power-control-machine machine-power-control-machine"],
	)

	for operationID, path := range commandPathAliases {
		assert.Equalf(t,
			operationID,
			operations[strings.Join(path, " ")],
			"concise command path for %q does not identify the same operation",
			operationID,
		)
	}
	for operationID, paths := range additionalCommandPathAliases {
		for _, path := range paths {
			assert.Equalf(t,
				operationID,
				operations[strings.Join(path, " ")],
				"additional command path for %q does not identify the same operation",
				operationID,
			)
		}
	}
}

func TestGeneratedCommandInfos_DomainAndSubnetSurface(t *testing.T) {
	spec, err := ParseSpec(openapi.Spec)
	require.NoError(t, err)

	infos := make(map[string]GeneratedCommandInfo)
	for _, info := range GeneratedCommandInfos(spec) {
		infos[info.Name] = info
	}

	for name, operationID := range map[string]string{
		"domain create":     "create-domain",
		"domain list":       "get-all-domain",
		"domain get":        "get-domain",
		"domain update":     "update-domain",
		"domain delete":     "delete-domain",
		"subnet create":     "create-subnet",
		"subnet attach-vpc": "attach-vpc-to-subnet",
	} {
		info, ok := infos[name]
		require.Truef(t, ok, "generated command %q is missing", name)
		assert.Equal(t, operationID, info.OperationID)
	}

	for name, expectedFlags := range map[string][]string{
		"domain create":     {"name", "site-id"},
		"domain list":       {"site-id", "tenant-id", "page-number", "page-size", "order-by"},
		"domain update":     {"name"},
		"subnet create":     {"subdomain-id"},
		"subnet attach-vpc": {"vpc-id", "allow-replace"},
	} {
		flagNames := make(map[string]struct{})
		for _, flag := range infos[name].Flags {
			flagNames[flag.Name] = struct{}{}
		}
		for _, expectedFlag := range expectedFlags {
			assert.Containsf(t, flagNames, expectedFlag, "%s flag", name)
		}
	}
}

// TestBuildActionCommand_BodyPropertyFlags verifies body-property flag naming
// for reserved names and scalar-compatible, single-item arrays.
func TestBuildActionCommand_BodyPropertyFlags(t *testing.T) {
	one := 1
	two := 2
	spec := &Spec{
		Paths: map[string]PathItem{
			"/v2/org/{org}/nico/widget": {
				Post: &Operation{
					OperationID: "create-widget",
					Tags:        []string{"Widget"},
					Parameters: []Parameter{
						{
							Name:        "legacyFilter",
							In:          "query",
							Deprecated:  true,
							Description: "Legacy filter",
							Schema: &Schema{
								Type: "string",
							},
						},
					},
					RequestBody: &RequestBody{
						Content: map[string]MediaType{
							"application/json": {
								Schema: &Schema{
									Type: "object",
									Properties: map[string]*Schema{
										"name":     {Type: "string"},
										"data":     {Type: "string"},
										"dataFile": {Type: "string"},
										"output":   {Type: "string"},
										"all":      {Type: "boolean"},
										"legacyBodyParam": {
											Type:       "boolean",
											Deprecated: true,
										},
										"rackIds": {
											Type: "array",
											Items: &Schema{
												Type: "string",
											},
											MaxItems: &one,
										},
										"tagIds": {
											Type: "array",
											Items: &Schema{
												Type: "string",
											},
											MaxItems: &two,
										},
									},
									Required: []string{"name"},
								},
							},
						},
					},
				},
			},
		},
	}

	ro := resolvedOp{
		tag:    "Widget",
		action: "create",
		method: "POST",
		path:   "/v2/org/{org}/nico/widget",
		op:     spec.Paths["/v2/org/{org}/nico/widget"].Post,
	}

	cmd := buildActionCommand(spec, ro, "")

	counts := make(map[string]int)
	for _, f := range cmd.Flags {
		counts[f.Names()[0]]++
	}

	// Wrapper flags stay unprefixed with exactly one registration each.
	assert.Equal(t, 1, counts["data"], "--data (JSON wrapper)")
	assert.Equal(t, 1, counts["data-file"], "--data-file (JSON wrapper)")
	assert.Equal(t, 1, counts["output"], "--output (format flag)")

	// --all is list-only; on a create action neither the wrapper nor the
	// colliding body property should be registered as plain --all.
	assert.Equal(t, 0, counts["all"], "--all should not be present for create")

	// Colliding body properties get a body- prefix.
	assert.Equal(t, 1, counts["body-data"], "--body-data (prefixed body property)")
	assert.Equal(t, 1, counts["body-data-file"], "--body-data-file (prefixed body property)")
	assert.Equal(t, 1, counts["body-output"], "--body-output (prefixed body property)")
	assert.Equal(t, 1, counts["body-all"], "--body-all (prefixed body property)")

	// Non-colliding body property stays unprefixed.
	assert.Equal(t, 1, counts["name"], "--name (non-reserved body property)")

	// A primitive array constrained to one item is presented as a scalar flag.
	// Arrays that permit multiple items still require JSON input.
	assert.Equal(t, 1, counts["rack-ids"], "--rack-ids (single-item array property)")
	assert.Equal(t, 0, counts["tag-ids"], "multi-item arrays do not get scalar flags")

	app := &cli.App{
		Name: "nicocli",
		Commands: []*cli.Command{
			{
				Name: "widget",
				Subcommands: []*cli.Command{
					cmd,
				},
			},
		},
	}
	var output bytes.Buffer
	app.Writer = &output
	err := app.Run([]string{
		"nicocli",
		"widget",
		"create",
		"-h",
	})
	require.NoError(t, err)
	assert.Equal(
		t,
		"--legacy-body-param value legacyBodyParam (deprecated)",
		normalizedOptionHelpLine(output.String(), "--legacy-body-param value"),
	)
	assert.Equal(
		t,
		"--legacy-filter value legacyFilter (deprecated): Legacy filter",
		normalizedOptionHelpLine(output.String(), "--legacy-filter value"),
	)
	assert.Equal(
		t,
		"--rack-ids value rackIds",
		normalizedOptionHelpLine(output.String(), "--rack-ids value"),
	)
	assert.NotContains(t, output.String(), "[ --rack-ids value ]")
}

func TestBuildRequestBody(t *testing.T) {
	var body []byte
	app := &cli.App{
		Flags: []cli.Flag{
			&cli.StringFlag{
				Name: "data",
			},
			&cli.StringFlag{
				Name: "data-file",
			},
			&cli.StringFlag{
				Name: "resource-ids",
			},
		},
		Action: func(c *cli.Context) error {
			var err error
			body, err = buildRequestBody(c, []bodyField{
				{
					jsonName: "resourceIds",
					flagName: "resource-ids",
					schema: &Schema{
						Type: "array",
					},
					wrapInList: true,
					itemType:   "string",
				},
			})
			return err
		},
	}

	err := app.Run([]string{
		"test",
		"--resource-ids",
		"resource-1",
	})
	require.NoError(t, err)
	assert.JSONEq(t, `{"resourceIds":["resource-1"]}`, string(body))
}

// TestNewApp_DpuExtensionServiceCreate_DoesNotPanic loads the real embedded
// OpenAPI spec and runs `nicocli dpu-extension-service create --help`. Prior
// to the reserved-flag fix this invocation panics with
// "create flag redefined: data" during urfave/cli flag-set construction.
func TestNewApp_DpuExtensionServiceCreate_DoesNotPanic(t *testing.T) {
	app, err := NewApp(openapi.Spec)
	require.NoError(t, err, "NewApp failed")

	require.NotPanics(t, func() {
		require.NoError(t, app.Run([]string{"nicocli", "dpu-extension-service", "create", "--help"}))
	})
}

// TestBuildActionCommand_UsageTextUsesBinaryName guards against regressing the
// dynamic usage string back to the literal "cli" prefix. Per-command --help
// output renders UsageText, so a wrong prefix shows up as
// "USAGE: cli site list" even though the binary is nicocli.
func TestBuildActionCommand_UsageTextUsesBinaryName(t *testing.T) {
	spec := &Spec{
		Paths: map[string]PathItem{
			"/v2/org/{org}/nico/site/{siteId}": {
				Get: &Operation{
					OperationID: "get-site",
					Tags:        []string{"Site"},
					Parameters: []Parameter{
						{Name: "siteId", In: "path"},
					},
				},
			},
		},
	}

	ro := resolvedOp{
		tag:    "Site",
		action: "get",
		method: "GET",
		path:   "/v2/org/{org}/nico/site/{siteId}",
		op:     spec.Paths["/v2/org/{org}/nico/site/{siteId}"].Get,
	}

	cmd := buildActionCommand(spec, ro, "")
	assert.Equal(t, "nicocli site get [command options] <siteId>", cmd.UsageText)
	assert.False(t, strings.HasPrefix(cmd.UsageText, "cli "),
		"UsageText must not start with the literal word 'cli '; got %q", cmd.UsageText)
}

func TestNewApp_RackGetUsageShowsOptionsBeforeID(t *testing.T) {
	app, err := NewApp(openapi.Spec)
	require.NoError(t, err)

	var output bytes.Buffer
	app.Writer = &output
	require.NoError(t, app.Run([]string{"nicocli", "rack", "get", "--help"}))

	assert.Contains(t, output.String(), "USAGE:\n   nicocli rack get [command options] <id>")
	assert.NotContains(t, output.String(), "nicocli rack get <id>")
}

// TestBuildCommands_AllUsageTextStartsWithBinaryName walks every dynamically
// built command from the embedded spec and asserts that the per-command
// UsageText begins with the actual binary name. This is the broadest form of
// the regression -- every leaf command renders UsageText in --help.
func TestBuildCommands_AllUsageTextStartsWithBinaryName(t *testing.T) {
	spec, err := ParseSpec(openapi.Spec)
	require.NoError(t, err, "ParseSpec failed")

	cmds := BuildCommands(spec)
	require.NotEmpty(t, cmds, "BuildCommands returned no commands")

	var visit func(path string, children []*cli.Command)
	visit = func(path string, children []*cli.Command) {
		for _, c := range children {
			full := path + " " + c.Name
			if c.UsageText != "" {
				assert.Truef(t, strings.HasPrefix(c.UsageText, "nicocli "),
					"command %q UsageText %q must start with %q",
					full, c.UsageText, "nicocli ")
			}
			if len(c.Subcommands) > 0 {
				visit(full, c.Subcommands)
			}
		}
	}
	visit("nicocli", cmds)
}

func TestDetectMisorderedFlags(t *testing.T) {
	usage := "nicocli machine update [command options] <machineId>"
	tests := []struct {
		name         string
		args         []string
		argParams    []string
		wantErr      bool
		wantContains []string
	}{
		{
			name:      "happy path - exactly the positional, no extras",
			args:      []string{"fm100htq"},
			argParams: []string{"machineId"},
			wantErr:   false,
		},
		{
			name:      "happy path - no positionals required and none given",
			args:      nil,
			argParams: nil,
			wantErr:   false,
		},
		{
			name:         "flag after positional - NVBug repro",
			args:         []string{"fm100htq", "--data", "{}"},
			argParams:    []string{"machineId"},
			wantErr:      true,
			wantContains: []string{"--data", "placed after a positional", "Move all flags before positionals", "[command options] <machineId>"},
		},
		{
			name:         "flag=value form after positional",
			args:         []string{"fm100htq", "--data={}"},
			argParams:    []string{"machineId"},
			wantErr:      true,
			wantContains: []string{"--data", "placed after a positional"},
		},
		{
			name:         "short flag after positional",
			args:         []string{"fm100htq", "-o", "yaml"},
			argParams:    []string{"machineId"},
			wantErr:      true,
			wantContains: []string{"-o"},
		},
		{
			name:         "multiple flags after positional",
			args:         []string{"fm100htq", "--data", "{}", "--output", "yaml"},
			argParams:    []string{"machineId"},
			wantErr:      true,
			wantContains: []string{"--data", "--output"},
		},
		{
			name:         "per-field flag after positional",
			args:         []string{"fm100htq", "--instance-type-id", "uuid-here"},
			argParams:    []string{"machineId"},
			wantErr:      true,
			wantContains: []string{"--instance-type-id"},
		},
		{
			name:         "extra positional without leading dash",
			args:         []string{"fm100htq", "bonusId"},
			argParams:    []string{"machineId"},
			wantErr:      true,
			wantContains: []string{"unexpected positional argument", "bonusId"},
		},
		{
			name:         "extra positional plus misplaced flag",
			args:         []string{"fm100htq", "bonusId", "--data", "{}"},
			argParams:    []string{"machineId"},
			wantErr:      true,
			wantContains: []string{"--data", "bonusId", "unexpected positional"},
		},
		{
			name:         "lone dash is not treated as a flag",
			args:         []string{"fm100htq", "-"},
			argParams:    []string{"machineId"},
			wantErr:      true,
			wantContains: []string{"unexpected positional", "-"},
		},
		{
			name:      "multi-positional command with flags in correct order",
			args:      []string{"instanceTypeId-1", "machineAssociationId-1"},
			argParams: []string{"instanceTypeId", "machineAssociationId"},
			wantErr:   false,
		},
		{
			name:         "multi-positional command with flag inside a required positional slot",
			args:         []string{"instanceTypeId-1", "--data={}"},
			argParams:    []string{"instanceTypeId", "machineAssociationId"},
			wantErr:      true,
			wantContains: []string{"--data", "placed after a positional"},
		},
		{
			name:         "multi-positional command with trailing flag",
			args:         []string{"instanceTypeId-1", "machineAssociationId-1", "--data", "{}"},
			argParams:    []string{"instanceTypeId", "machineAssociationId"},
			wantErr:      true,
			wantContains: []string{"--data"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := detectMisorderedFlagsInArgs(tt.args, tt.argParams, usage)
			if tt.wantErr && err == nil {
				t.Fatalf("expected error, got nil")
			}
			if !tt.wantErr && err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if err != nil {
				for _, want := range tt.wantContains {
					if !strings.Contains(err.Error(), want) {
						t.Errorf("error missing %q:\n%s", want, err.Error())
					}
				}
			}
		})
	}
}

func TestIsActionModifier(t *testing.T) {
	tests := []struct {
		input string
		want  bool
	}{
		{"status-history", true},
		{"stats", true},
		{"constraint", false},
		{"version", false},
		{"virtualization", false},
		{"machine-association", false},
	}

	for _, tt := range tests {
		t.Run(tt.input, func(t *testing.T) {
			got := isActionModifier(tt.input)
			if got != tt.want {
				t.Errorf("isActionModifier(%q) = %v, want %v", tt.input, got, tt.want)
			}
		})
	}
}

// TestBuildTagSubcommands_AliasCollisionExpandsAllNames documents the fix
// for the alias-collision determinism bug. When two operations under the
// same tag collapse to the same short action name (e.g.
// `get-current-infrastructure-provider` and
// `get-current-infrastructure-provider-stats` both -> `get`), the generated
// command tree must expose BOTH operations under their full OperationID and
// must NOT expose either under the colliding short name. Without this, the
// short alias non-deterministically points at one of the two operations
// depending on map iteration order, so the same binary exposes a different
// command surface depending on whether the config file was loaded.
//
// NOTE: this exercises the collision resolver with synthetic ops (action
// "get" injected on both). In the embedded spec these two operations now map
// to distinct `current` / `stats` actions, so this exact collision no longer
// arises there; the live singleton-surface guard is
// TestNewApp_CurrentSingletonCommandSurface.
func TestBuildTagSubcommands_AliasCollisionExpandsAllNames(t *testing.T) {
	infraProviderGet := &Operation{
		OperationID: "get-current-infrastructure-provider",
		Tags:        []string{"Infrastructure Provider"},
		Summary:     "Retrieve Infrastructure Provider for current Org",
	}
	infraProviderStats := &Operation{
		OperationID: "get-current-infrastructure-provider-stats",
		Tags:        []string{"Infrastructure Provider"},
		Summary:     "Retrieve Stats for current Infrastructure Provider",
	}
	ops := []resolvedOp{
		{tag: "Infrastructure Provider", action: "get", method: "GET", path: "/p1", op: infraProviderGet},
		{tag: "Infrastructure Provider", action: "get", method: "GET", path: "/p2", op: infraProviderStats},
	}

	cmds := buildTagSubcommands(&Spec{}, ops)

	names := make(map[string]int)
	for _, c := range cmds {
		names[c.Name]++
	}
	assert.Equal(t, 0, names["get"],
		"colliding short alias must be dropped entirely, not assigned to one operation non-deterministically")
	assert.Equal(t, 1, names["get-current-infrastructure-provider"])
	assert.Equal(t, 1, names["get-current-infrastructure-provider-stats"])
}

// TestBuildTagSubcommands_NonCollidingActionKeepsShortAlias is the negative
// counterpart to the collision test above: when there is exactly one
// operation per short action, the short alias is preserved. Uses a plain
// get-<resource> op (a genuine `get` action) so the fixture is self-consistent
// -- get-current-* singletons now map to `current`, not `get`.
func TestBuildTagSubcommands_NonCollidingActionKeepsShortAlias(t *testing.T) {
	op := &Operation{
		OperationID: "get-site",
		Tags:        []string{"Site"},
	}
	ops := []resolvedOp{
		{tag: "Site", action: "get", method: "GET", path: "/p1", op: op},
	}

	cmds := buildTagSubcommands(&Spec{}, ops)

	require.Len(t, cmds, 1)
	assert.Equal(t, "get", cmds[0].Name,
		"a single-op tag must keep its short alias; collision-expansion must not over-fire")
}

// TestBuildTagSubcommands_AliasCollisionIsOrderIndependent simulates the two
// states the bug filer observed (config-loaded vs config-not-loaded) by
// running the resolver against both possible orderings of the colliding
// operations. The resulting command tree must be identical, so the binary's
// command surface no longer depends on Go map iteration order.
//
// NOTE: synthetic action "get" is injected on both ops to force the collision;
// in the embedded spec these resolve to distinct `current` / `stats` actions.
func TestBuildTagSubcommands_AliasCollisionIsOrderIndependent(t *testing.T) {
	infraProviderGet := &Operation{
		OperationID: "get-current-infrastructure-provider",
		Tags:        []string{"Infrastructure Provider"},
	}
	infraProviderStats := &Operation{
		OperationID: "get-current-infrastructure-provider-stats",
		Tags:        []string{"Infrastructure Provider"},
	}

	collectNames := func(ops []resolvedOp) []string {
		cmds := buildTagSubcommands(&Spec{}, ops)
		names := make([]string, 0, len(cmds))
		for _, c := range cmds {
			names = append(names, c.Name)
		}
		// Sort because primaryOps slice order is map-iteration-derived; we
		// only care that the *set* of names is identical across orderings.
		sortedNames := append([]string(nil), names...)
		sortStrings(sortedNames)
		return sortedNames
	}

	forward := collectNames([]resolvedOp{
		{tag: "Infrastructure Provider", action: "get", method: "GET", path: "/p1", op: infraProviderGet},
		{tag: "Infrastructure Provider", action: "get", method: "GET", path: "/p2", op: infraProviderStats},
	})
	reverse := collectNames([]resolvedOp{
		{tag: "Infrastructure Provider", action: "get", method: "GET", path: "/p2", op: infraProviderStats},
		{tag: "Infrastructure Provider", action: "get", method: "GET", path: "/p1", op: infraProviderGet},
	})
	assert.Equal(t, forward, reverse,
		"command surface must not depend on the order primaryOps is built in")
}

// TestNewApp_CurrentSingletonCommandSurface walks the embedded production spec
// and asserts that the get-current-<resource> singletons expose the `current`
// action (and `stats` for their -stats endpoints) instead of the bare `get`
// action or the full operationId. Because `current` and `stats` are distinct
// actions there is no get-action collision, so the command surface is
// deterministic and never depends on Go map iteration order. This is the
// regression guard for NVBug 6100988 (`tenant current` must be runnable from
// the command line, matching what the interactive TUI prints) and replaces the
// old guard that asserted the colliding pair expanded to full operationIds.
func TestNewApp_CurrentSingletonCommandSurface(t *testing.T) {
	app, err := NewApp(openapi.Spec)
	require.NoError(t, err, "NewApp failed")

	// tag -> actions that must be present, with no bare `get` and no
	// full-operationId leftovers.
	want := map[string][]string{
		"infrastructure-provider": {"current", "stats"},
		"tenant":                  {"current", "stats"},
	}

	for tag, actions := range want {
		t.Run(tag, func(t *testing.T) {
			var found *cli.Command
			for _, c := range app.Commands {
				if c.Name == tag {
					found = c
					break
				}
			}
			require.NotNilf(t, found, "tag %q should be present in the generated command tree", tag)

			subNames := make(map[string]bool)
			for _, sc := range found.Subcommands {
				subNames[sc.Name] = true
			}
			for _, a := range actions {
				assert.Truef(t, subNames[a], "tag %q must expose the %q command", tag, a)
			}
			assert.Falsef(t, subNames["get"],
				"tag %q must NOT expose a bare `get`; the singleton getter is `current`", tag)
			assert.Falsef(t, subNames["get-current-"+tag],
				"tag %q must NOT expose the full-operationId command; it collapses to `current`", tag)
		})
	}
}

func TestNewApp_UEFICredentialCreateCommand(t *testing.T) {
	app, err := NewApp(openapi.Spec)
	require.NoError(t, err, "NewApp failed")

	var credential *cli.Command
	for _, command := range app.Commands {
		if command.Name == "uefi-credential" {
			credential = command
			break
		}
	}
	require.NotNil(t, credential, "UEFI credential must be exposed by the embedded OpenAPI spec")

	var create *cli.Command
	for _, command := range credential.Subcommands {
		if command.Name == "create" {
			create = command
			break
		}
	}
	require.NotNil(t, create, "UEFI credential must expose a create command")
}

// TestBuildCommands_CurrentSingletonsAreRunnable asserts that every
// get-current-<resource> singleton in the embedded spec is reachable from the
// non-interactive CLI under the `current` action that the interactive TUI
// prints (NVBug 6100988). Driven off the spec so it stays honest as singletons
// are added or removed.
func TestBuildCommands_CurrentSingletonsAreRunnable(t *testing.T) {
	spec, err := ParseSpec(openapi.Spec)
	require.NoError(t, err)
	cmds := BuildCommands(spec)

	cmdByName := func(list []*cli.Command, name string) *cli.Command {
		for _, c := range list {
			if c.HasName(name) {
				return c
			}
		}
		return nil
	}

	for _, tag := range []string{"tenant", "infrastructure-provider", "service-account"} {
		t.Run(tag, func(t *testing.T) {
			parent := cmdByName(cmds, tag)
			require.NotNilf(t, parent, "tag %q must be a top-level command", tag)
			assert.NotNilf(t, cmdByName(parent.Subcommands, "current"),
				"tag %q must expose a `current` command runnable from the CLI", tag)
		})
	}
}

func TestBuildCommands_SiteExplorerActionIsRunnable(t *testing.T) {
	spec, err := ParseSpec(openapi.Spec)
	require.NoError(t, err)

	var siteExplorer *cli.Command
	for _, command := range BuildCommands(spec) {
		if command.HasName("site-explorer") {
			siteExplorer = command
			break
		}
	}
	require.NotNil(t, siteExplorer)

	var sawCreate, sawEndpointAction, sawList bool
	for _, command := range siteExplorer.Subcommands {
		if command.HasName("create") {
			sawCreate = true
		}
		if command.HasName("endpoint") && len(command.Subcommands) > 0 {
			for _, subcommand := range command.Subcommands {
				if subcommand.HasName("action") && subcommand.Action != nil {
					sawEndpointAction = true
				}
			}
		}
		if command.HasName("list") {
			sawList = true
		}
	}
	if !sawCreate {
		t.Fatal("site-explorer create must be generated from the OpenAPI operation")
	}
	require.True(t, sawEndpointAction,
		"site-explorer endpoint action must be generated from the OpenAPI operation")
	if !sawList {
		t.Fatal("site-explorer list must be generated from the OpenAPI operation")
	}
}

// TestBuildCommands_AllocationConstraintIsUpdateOnly is the CLI-side guard for
// NVBug 6232163: the server only registers PATCH for the AllocationConstraint
// sub-resource, and the stale create/get/list/delete endpoints were removed
// from the OpenAPI spec. Because the CLI is generated from the embedded spec,
// the `allocation constraint` sub-resource must therefore expose only `update`.
// This test fails if the removed endpoints are ever reintroduced into the spec.
func TestBuildCommands_AllocationConstraintIsUpdateOnly(t *testing.T) {
	spec, err := ParseSpec(openapi.Spec)
	require.NoError(t, err)
	cmds := BuildCommands(spec)

	var allocation *cli.Command
	for _, c := range cmds {
		if c.Name == "allocation" {
			allocation = c
			break
		}
	}
	require.NotNil(t, allocation, "allocation command must exist")

	var constraint *cli.Command
	for _, sc := range allocation.Subcommands {
		if sc.Name == "constraint" {
			constraint = sc
			break
		}
	}
	require.NotNil(t, constraint, "allocation `constraint` sub-resource must exist")

	actions := make([]string, 0, len(constraint.Subcommands))
	for _, sc := range constraint.Subcommands {
		actions = append(actions, sc.Name)
	}
	assert.Equal(t, []string{"update"}, actions,
		"allocation constraint must expose only `update`; create/get/list/delete were "+
			"removed from the OpenAPI spec because the server never registered those routes (NVBug 6232163)")
}

func TestNewApp_MachineValidationStartCommandSurface(t *testing.T) {
	app, err := NewApp(openapi.Spec)
	require.NoError(t, err)

	var machineValidation *cli.Command
	for _, command := range app.Commands {
		if command.Name == "machine-validation" {
			machineValidation = command
			break
		}
	}
	require.NotNil(t, machineValidation, "Machine validation must be exposed by the embedded OpenAPI spec")

	var start *cli.Command
	for _, command := range machineValidation.Subcommands {
		if command.Name == "start" {
			start = command
			break
		}
	}
	require.NotNil(t, start, "Machine validation must expose a start command")
	assert.Equal(t, "nicocli machine-validation start [command options] <machineId>", start.UsageText)

	for _, resourceName := range []string{"results", "runs"} {
		var resource *cli.Command
		for _, command := range machineValidation.Subcommands {
			if command.Name == resourceName {
				resource = command
				break
			}
		}
		require.NotNil(t, resource, "Machine validation must expose a %s sub-resource", resourceName)

		var list *cli.Command
		for _, command := range resource.Subcommands {
			if command.Name == "list" {
				list = command
				break
			}
		}
		require.NotNil(t, list, "Machine validation %s must expose a list command", resourceName)
	}
}

func TestNewApp_MachineValidationStartExecutesRESTRequest(t *testing.T) {
	var method, path, authorization, body string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
		method = request.Method
		path = request.URL.Path
		authorization = request.Header.Get("Authorization")
		requestBody, err := io.ReadAll(request.Body)
		require.NoError(t, err)
		body = string(requestBody)
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusAccepted)
		_, err = w.Write([]byte(`{"validationID":"validation-1","machineID":"machine-1","startTime":"2026-08-05T16:00:00Z","endTime":null,"name":"Test_machine-1","context":"OnDemand","status":{"state":"Started","total":0,"completed":0},"durationToCompleteSecs":0}`))
		require.NoError(t, err)
	}))
	t.Cleanup(server.Close)

	app, err := NewApp(openapi.Spec)
	require.NoError(t, err)

	err = app.Run([]string{
		"nicocli",
		"--base-url", server.URL,
		"--org", "test-org",
		"--api-name", "nico",
		"--token", "test-token",
		"machine-validation", "start",
		"--data", `{"allowedTests":["gpu_bandwidth"],"runUnverifiedTests":true}`,
		"machine-1",
	})
	require.NoError(t, err)
	assert.Equal(t, http.MethodPost, method)
	assert.Equal(t, "/v2/org/test-org/nico/machine/machine-1/validation/run", path)
	assert.Equal(t, "Bearer test-token", authorization)
	assert.JSONEq(t, `{"allowedTests":["gpu_bandwidth"],"runUnverifiedTests":true}`, body)
}

func normalizedOptionHelpLine(output, option string) string {
	for _, line := range strings.Split(output, "\n") {
		normalized := strings.Join(strings.Fields(line), " ")
		if strings.HasPrefix(normalized, option) {
			return normalized
		}
	}
	return ""
}

func TestNewApp_MachineValidationReadCommandsExecuteRESTRequests(t *testing.T) {
	tests := []struct {
		name         string
		resource     string
		expectedPath string
	}{
		{
			name:         "runs",
			resource:     "runs",
			expectedPath: "/v2/org/test-org/nico/machine/machine-1/validation/run",
		},
		{
			name:         "results",
			resource:     "results",
			expectedPath: "/v2/org/test-org/nico/machine/machine-1/validation/result",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var method, path, authorization string
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
				method = request.Method
				path = request.URL.Path
				authorization = request.Header.Get("Authorization")
				w.Header().Set("Content-Type", "application/json")
				_, err := w.Write([]byte(`[]`))
				require.NoError(t, err)
			}))
			t.Cleanup(server.Close)

			app, err := NewApp(openapi.Spec)
			require.NoError(t, err)

			err = app.Run([]string{
				"nicocli",
				"--base-url", server.URL,
				"--org", "test-org",
				"--api-name", "nico",
				"--token", "test-token",
				"machine-validation", tt.resource, "list",
				"machine-1",
			})
			require.NoError(t, err)
			assert.Equal(t, http.MethodGet, method)
			assert.Equal(t, tt.expectedPath, path)
			assert.Equal(t, "Bearer test-token", authorization)
		})
	}
}

// sortStrings is a tiny stable sort used by the order-independence test so it
// stays self-contained and does not pull in sort.Strings (which is already
// used elsewhere; this just keeps the test readable).
func sortStrings(s []string) {
	for i := 1; i < len(s); i++ {
		for j := i; j > 0 && s[j-1] > s[j]; j-- {
			s[j-1], s[j] = s[j], s[j-1]
		}
	}
}
