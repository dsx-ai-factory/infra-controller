// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"sort"
	"strings"
	"sync/atomic"
	"testing"

	appcli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
	"github.com/NVIDIA/infra-controller/rest-api/openapi"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestQuoteShellCommandArgument(t *testing.T) {
	for _, test := range []struct {
		name  string
		value string
		want  string
	}{
		{name: "empty", want: "''"},
		{name: "safe characters", value: "abc-123_@%+=:,./", want: "abc-123_@%+=:,./"},
		{name: "whitespace", value: "two words", want: "'two words'"},
		{name: "single quote", value: "single'quote", want: "'single'\"'\"'quote'"},
		{name: "backslash", value: "path\\name", want: "'path\\name'"},
		{name: "control character", value: "line\nbreak", want: "'line\nbreak'"},
		{name: "non-ASCII", value: "caf\u00e9", want: "'caf\u00e9'"},
		{name: "shell metacharacters", value: "$HOME;rm", want: "'$HOME;rm'"},
	} {
		t.Run(test.name, func(t *testing.T) {
			assert.Equal(t, test.want, quoteShellCommandArgument(test.value))
		})
	}
}

func TestAllCommands_CoversGeneratedCLISurface(t *testing.T) {
	spec, err := appcli.ParseSpec(openapi.Spec)
	require.NoError(t, err)

	infos := appcli.GeneratedCommandInfos(spec)
	generatedNames := make([]string, 0, len(infos))
	for _, info := range infos {
		generatedNames = append(generatedNames, info.Name)
	}
	generated := make(map[string]struct{}, len(generatedNames))
	for _, name := range generatedNames {
		generated[name] = struct{}{}
	}

	tuiCommands := make(map[string]struct{}, len(AllCommands()))
	for _, command := range AllCommands() {
		tuiCommands[command.Name] = struct{}{}
	}

	for source, target := range generatedCommandAliases {
		_, sourceExists := generated[source]
		assert.Truef(t, sourceExists, "alias source %q is not a generated CLI command", source)
		_, targetExists := tuiCommands[target]
		assert.Truef(t, targetExists, "alias target %q is not a TUI command", target)
		_, sourceAvailable := tuiCommands[source]
		assert.Truef(t, sourceAvailable, "generated source %q is not a TUI command", source)
		_, alsoExcluded := generatedCommandExclusions[source]
		assert.Falsef(t, alsoExcluded, "generated command %q is both aliased and excluded", source)
	}
	for name, reason := range generatedCommandExclusions {
		_, exists := generated[name]
		assert.Truef(t, exists, "excluded command %q is not a generated CLI command", name)
		assert.NotEmptyf(t, strings.TrimSpace(reason), "excluded command %q needs a reviewed reason", name)
	}

	var missing []string
	for _, name := range generatedNames {
		if _, exists := tuiCommands[name]; exists {
			continue
		}
		if _, excluded := generatedCommandExclusions[name]; excluded {
			continue
		}
		missing = append(missing, name)
	}
	sort.Strings(missing)
	assert.Empty(t, missing, "generated CLI commands missing from the TUI")
}

func TestAllCommands_RegistersConciseAliases(t *testing.T) {
	commands := commandNames(AllCommands())
	for _, name := range []string{
		"machine power",
		"machine power-control-machine machine-power-control-machine",
		"measured-boot machine approve",
		"measured-boot machine list",
		"measured-boot machine remove",
		"measured-boot profile approve",
		"measured-boot profile list",
		"measured-boot profile remove",
		"site-explorer endpoint action",
	} {
		t.Run(name, func(t *testing.T) {
			assert.Containsf(t, commands, name, "expected concise command %q", name)
		})
	}
}

func TestMachinePowerAliasesExecuteSameOperation(t *testing.T) {
	for _, name := range []string{
		"machine power",
		"machine power-control-machine machine-power-control-machine",
	} {
		t.Run(name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				assert.Equal(t, http.MethodPatch, r.Method)
				assert.Equal(t, "/v2/org/acme/nico/machine/machine-1/power", r.URL.Path)
				body, err := io.ReadAll(r.Body)
				require.NoError(t, err)
				assert.JSONEq(t, `{"action":"ForceRestart"}`, string(body))

				w.Header().Set("Content-Type", "application/json")
				_, err = io.WriteString(w, `{"status":"accepted"}`)
				require.NoError(t, err)
			}))
			defer server.Close()

			client := appcli.NewClient(server.URL, "acme", "token", nil, false)
			session := &Session{Client: client, Cache: NewCache()}
			command := requireTUICommand(t, name)

			_, err := withStdin(t, "y\n", func() (string, error) {
				var runErr error
				output := captureStdout(func() {
					runErr = command.Run(session, []string{
						"--action", "ForceRestart", "machine-1",
					})
				})
				return output, runErr
			})
			require.NoError(t, err)
		})
	}
}

func TestAllCommands_RegistersRepresentativeFormerGaps(t *testing.T) {
	commands := commandNames(AllCommands())
	for _, name := range []string{
		"allocation constraint update",
		"bmc-credential create",
		"dpu-extension-service version get",
		"expected-machine batch-create",
		"health-report list",
		"instance-type machine-association create",
		"ip-block derived list",
		"ipxe-template list",
		"machine capabilities list",
		"rack bringup-racks bringup-racks",
		"rule list-rules list-rules",
		"site-explorer create",
		"site-explorer list",
		"uefi-credential create",
		"vpc-peering list",
	} {
		assert.Containsf(t, commands, name, "expected generated fallback %q", name)
	}
}

func TestAppendGeneratedCommandInfos_RegistersFutureOperation(t *testing.T) {
	spec, err := appcli.ParseSpec([]byte(`
info:
  title: test
  version: test
paths:
  /v2/org/{org}/nico/trusted-machine:
    get:
      tags: [Trusted Machine]
      summary: List trusted machines
      operationId: get-all-trusted-machine
`))
	require.NoError(t, err)

	commands := appendGeneratedCommandInfos(
		[]Command{{Name: "help", Run: cmdHelp}},
		appcli.GeneratedCommandInfos(spec),
	)

	require.Len(t, commands, 2)
	assert.Equal(t, "trusted-machine list", commands[1].Name)
	assert.NotNil(t, commands[1].Run)
}

func TestAppendGeneratedCommandInfos_CoversPendingMeasuredBootOperations(t *testing.T) {
	spec, err := appcli.ParseSpec([]byte(`
info:
  title: measured boot parity
  version: test
paths:
  /v2/org/{org}/nico/measured-boot/trusted-machine:
    post:
      tags: [Measured Boot Trusted Machine]
      operationId: create-measurement-trusted-machine
    get:
      tags: [Measured Boot Trusted Machine]
      operationId: get-all-measurement-trusted-machine
  /v2/org/{org}/nico/measured-boot/trusted-machine/{id}:
    delete:
      tags: [Measured Boot Trusted Machine]
      operationId: delete-measurement-trusted-machine
  /v2/org/{org}/nico/measured-boot/trusted-profile:
    post:
      tags: [Measured Boot Trusted Profile]
      operationId: create-measurement-trusted-profile
    get:
      tags: [Measured Boot Trusted Profile]
      operationId: get-all-measurement-trusted-profile
  /v2/org/{org}/nico/measured-boot/trusted-profile/{id}:
    delete:
      tags: [Measured Boot Trusted Profile]
      operationId: delete-measurement-trusted-profile
`))
	require.NoError(t, err)

	commands := appendGeneratedCommandInfos(nil, appcli.GeneratedCommandInfos(spec))
	names := commandNames(commands)
	for _, name := range []string{
		"measured-boot-trusted-machine create",
		"measured-boot-trusted-machine list",
		"measured-boot-trusted-machine delete",
		"measured-boot-trusted-profile create",
		"measured-boot-trusted-profile list",
		"measured-boot-trusted-profile delete",
	} {
		assert.Contains(t, names, name)
	}
}

func TestGeneratedCommand_ReadOnlyUsesSessionClientScopeAndFetchesAll(t *testing.T) {
	type request struct {
		path          string
		query         string
		authorization string
	}
	requests := make(chan request, 1)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests <- request{
			path:          r.URL.Path,
			query:         r.URL.RawQuery,
			authorization: r.Header.Get("Authorization"),
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(w, `[{"type":"GPU","name":"H100"}]`)
	}))
	defer server.Close()

	client := appcli.NewClient(server.URL, "acme", "session-token", nil, false)
	client.APIName = "custom-api"
	session := NewSession(client, "acme", "/tmp/config with spaces.yaml")
	session.Scope.SiteID = "site-1"

	command := requireTUICommand(t, "machine capabilities list")
	var runErr error
	output := captureStdout(func() {
		runErr = command.Run(session, nil)
	})
	require.NoError(t, runErr)

	got := <-requests
	assert.Equal(t, "/v2/org/acme/custom-api/machine-capability", got.path)
	assert.Contains(t, got.query, "siteId=site-1")
	assert.Contains(t, got.query, "pageNumber=1")
	assert.Contains(t, got.query, "pageSize=100")
	assert.Equal(t, "Bearer session-token", got.authorization)
	assert.Contains(t, output, "nicocli")
	assert.Contains(t, output, "--site-id site-1")
	assert.Contains(t, output, "--all")
	assert.Contains(t, output, `"H100"`)
}

func TestGeneratedCommand_ExplicitPaginationIsNotOverridden(t *testing.T) {
	for _, test := range []struct {
		name           string
		args           []string
		wantPageNumber string
		wantPageSize   string
	}{
		{
			name:           "separate values",
			args:           []string{"--page-number", "3", "--page-size", "7"},
			wantPageNumber: "3",
			wantPageSize:   "7",
		},
		{
			name:           "inline values",
			args:           []string{"--page-number=4", "--page-size=9"},
			wantPageNumber: "4",
			wantPageSize:   "9",
		},
	} {
		t.Run(test.name, func(t *testing.T) {
			requestQuery := make(chan string, 1)
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				requestQuery <- r.URL.RawQuery
				w.Header().Set("Content-Type", "application/json")
				_, _ = io.WriteString(w, `[]`)
			}))
			defer server.Close()

			session := NewSession(
				appcli.NewClient(server.URL, "acme", "token", nil, false),
				"acme",
				"",
			)
			var runErr error
			output := captureStdout(func() {
				runErr = requireTUICommand(t, "vpc-peering list").Run(session, test.args)
			})
			require.NoError(t, runErr)

			query, err := url.ParseQuery(<-requestQuery)
			require.NoError(t, err)
			assert.Equal(t, test.wantPageNumber, query.Get("pageNumber"))
			assert.Equal(t, test.wantPageSize, query.Get("pageSize"))
			assert.NotContains(t, output, "--all")
		})
	}
}

func TestGeneratedCommand_PromptsForRequiredQuery(t *testing.T) {
	requestQuery := make(chan string, 1)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requestQuery <- r.URL.RawQuery
		w.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(w, `[]`)
	}))
	defer server.Close()

	client := appcli.NewClient(server.URL, "acme", "token", nil, false)
	session := &Session{Client: client, Cache: NewCache()}
	command := requireTUICommand(t, "machine capabilities list")

	_, err := withStdin(t, "site-from-prompt\n", func() (string, error) {
		return "", command.Run(session, nil)
	})
	require.NoError(t, err)
	assert.Contains(t, <-requestQuery, "siteId=site-from-prompt")
}

func TestGeneratedCommand_MutationConfirmsRedactsAndInvalidatesCache(t *testing.T) {
	type request struct {
		method        string
		path          string
		body          string
		authorization string
	}
	requests := make(chan request, 1)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		requests <- request{
			method:        r.Method,
			path:          r.URL.Path,
			body:          string(body),
			authorization: r.Header.Get("Authorization"),
		}
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusCreated)
		_, _ = io.WriteString(w, `{"siteId":"site-1","kind":"Host"}`)
	}))
	defer server.Close()

	client := appcli.NewClient(server.URL, "acme", "session-token", nil, false)
	cache := NewCache()
	cache.Set("site", []NamedItem{{Name: "Site One", ID: "site-1"}})
	session := &Session{
		Client:     client,
		ConfigPath: "/tmp/config.yaml",
		Cache:      cache,
		Scope:      Scope{SiteID: "site-1"},
	}
	command := requireTUICommand(t, "uefi-credential create")
	body := `{"kind":"Host","password":"super-secret"}`

	output, err := withStdin(t, "y\n", func() (string, error) {
		var runErr error
		out := captureStdout(func() {
			runErr = command.Run(session, []string{"--data", body})
		})
		return out, runErr
	})
	require.NoError(t, err)

	got := <-requests
	assert.Equal(t, http.MethodPost, got.method)
	assert.Equal(t, "/v2/org/acme/nico/credential/uefi", got.path)
	assert.JSONEq(t, `{"siteId":"site-1","kind":"Host","password":"super-secret"}`, got.body)
	assert.Equal(t, "Bearer session-token", got.authorization)
	assert.Contains(t, output, "Run uefi-credential create (POST)?")
	assert.Contains(t, output, "nicocli")
	assert.Contains(t, output, "--data '<redacted>'")
	assert.NotContains(t, output, "super-secret")
	assert.Nil(t, cache.Get("site"), "successful mutations must invalidate cached resources")
}

func TestGeneratedCommand_CancelledMutationDoesNotCallAPI(t *testing.T) {
	var calls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		calls.Add(1)
		w.WriteHeader(http.StatusNoContent)
	}))
	defer server.Close()

	client := appcli.NewClient(server.URL, "acme", "token", nil, false)
	cache := NewCache()
	cache.Set("machine", []NamedItem{{Name: "host-one", ID: "machine-1"}})
	session := &Session{Client: client, Cache: cache}
	command := requireTUICommand(t, "machine delete")

	output, err := withStdin(t, "n\nn\n", func() (string, error) {
		var runErr error
		out := captureStdout(func() {
			runErr = command.Run(session, []string{"machine-1"})
		})
		return out, runErr
	})
	require.NoError(t, err)
	assert.Zero(t, calls.Load())
	assert.NotNil(t, cache.Get("machine"), "cancelled mutations must preserve the cache")
	assert.Contains(t, output, "Force delete Machine machine-1?")
	assert.NotContains(t, output, "INFO:")
}

func TestGeneratedCommand_MachineDeleteOffersForceMode(t *testing.T) {
	for _, tc := range []struct {
		name            string
		args            []string
		input           string
		wantQuery       string
		wantForcePrompt bool
		cancel          bool
	}{
		{name: "guided force delete", args: []string{"machine-1"}, input: "y\ny\n", wantQuery: "force=true", wantForcePrompt: true},
		{name: "guided standard delete", args: []string{"machine-1"}, input: "n\ny\n", wantForcePrompt: true},
		{name: "explicit force flag", args: []string{"--force", "machine-1"}, input: "y\ny\n", wantQuery: "force=true", wantForcePrompt: true},
		{name: "explicit true value", args: []string{"--force=true", "machine-1"}, input: "y\ny\n", wantQuery: "force=true", wantForcePrompt: true},
		{name: "explicit false value", args: []string{"--force=false", "machine-1"}, input: "y\n", wantQuery: "force=false"},
		{name: "decline explicit force", args: []string{"--force", "machine-1"}, input: "n\n", wantForcePrompt: true, cancel: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			queries := make(chan string, 1)
			var calls atomic.Int32
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				calls.Add(1)
				queries <- r.URL.RawQuery
				w.Header().Set("Content-Type", "application/json")
				w.WriteHeader(http.StatusAccepted)
				_, _ = io.WriteString(w, `{"message":"Deletion request was accepted"}`)
			}))
			defer server.Close()

			cache := NewCache()
			cache.Set("machine", []NamedItem{{Name: "host-one", ID: "machine-1"}})
			session := &Session{
				Client: appcli.NewClient(server.URL, "acme", "token", nil, false),
				Cache:  cache,
			}
			command := requireTUICommand(t, "machine delete")
			output, err := withStdin(t, tc.input, func() (string, error) {
				var runErr error
				out := captureStdout(func() {
					runErr = command.Run(session, tc.args)
				})
				return out, runErr
			})
			require.NoError(t, err)
			assert.Equal(t, tc.wantForcePrompt, strings.Contains(output, "Force delete Machine machine-1?"))
			if tc.cancel {
				assert.Zero(t, calls.Load())
				assert.NotNil(t, cache.Get("machine"), "cancelled mutations must preserve the cache")
				assert.NotContains(t, output, "Run machine delete (DELETE)?")
				assert.NotContains(t, output, "INFO:")
				return
			}
			assert.Equal(t, int32(1), calls.Load())
			assert.Equal(t, tc.wantQuery, <-queries)
			assert.Contains(t, output, "Run machine delete (DELETE)?")
			assert.Contains(t, output, "Deletion request was accepted")
		})
	}
}

func TestGeneratedCommand_HelpLikeValuesDoNotBypassConfirmation(t *testing.T) {
	for name, args := range map[string][]string{
		"false help flag": {"--help=false", "--data", `{"kind":"Host"}`},
		"help-like value": {"--data", "--help"},
	} {
		t.Run(name, func(t *testing.T) {
			var calls atomic.Int32
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
				calls.Add(1)
				w.WriteHeader(http.StatusCreated)
			}))
			defer server.Close()

			session := &Session{
				Client: appcli.NewClient(server.URL, "acme", "token", nil, false),
				Cache:  NewCache(),
			}
			command := requireTUICommand(t, "uefi-credential create")

			output, err := withStdin(t, "n\n", func() (string, error) {
				var runErr error
				out := captureStdout(func() {
					runErr = command.Run(session, args)
				})
				return out, runErr
			})
			require.NoError(t, err)
			assert.Zero(t, calls.Load())
			assert.Contains(t, output, "Run uefi-credential create (POST)?")
			assert.NotContains(t, output, "USAGE:")
		})
	}
}

func TestGeneratedCommand_SensitiveNestedAndArrayBodiesRedactHistory(t *testing.T) {
	for _, name := range []string{
		"dpu-extension-service create",
		"expected-machine batch-create",
	} {
		info := requireGeneratedInfo(t, name)
		assert.True(t, generatedCommandHasSensitiveBody(info), name)

		command := requireTUICommand(t, name)
		assert.True(t, command.Sensitive, name)
		commandMap := map[string]Command{name: command}
		got := commandHistoryLine(name+` --data '{"password":"secret"}'`, commandMap, []Command{command})
		assert.Equal(t, name+" <redacted>", got)
	}
}

func TestGeneratedCommand_DataFileArrayInheritsSiteScope(t *testing.T) {
	requestBody := make(chan string, 1)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		requestBody <- string(body)
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusCreated)
		_, _ = io.WriteString(w, `[]`)
	}))
	defer server.Close()

	dataFile := t.TempDir() + "/expected-machines.json"
	require.NoError(t, os.WriteFile(
		dataFile,
		[]byte(`[{"bmcMacAddress":"00:11:22:33:44:55","defaultBmcPassword":"file-secret"}]`),
		0o600,
	))
	session := &Session{
		Client: appcli.NewClient(server.URL, "acme", "token", nil, false),
		Cache:  NewCache(),
		Scope:  Scope{SiteID: "site-from-scope"},
	}
	command := requireTUICommand(t, "expected-machine batch-create")

	output, err := withStdin(t, "y\n", func() (string, error) {
		var runErr error
		out := captureStdout(func() {
			runErr = command.Run(session, []string{"--data-file", dataFile})
		})
		return out, runErr
	})
	require.NoError(t, err)
	assert.JSONEq(t,
		`[{"siteId":"site-from-scope","bmcMacAddress":"00:11:22:33:44:55","defaultBmcPassword":"file-secret"}]`,
		<-requestBody,
	)
	assert.Contains(t, output, "--data '<redacted>'")
	assert.NotContains(t, output, "file-secret")
}

func TestGeneratedCommand_UnpaginatedListDoesNotFetchAll(t *testing.T) {
	var calls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		if calls.Add(1) > 1 {
			http.Error(w, "unexpected repeated request", http.StatusInternalServerError)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		items := "[" + strings.TrimSuffix(strings.Repeat(`{"machineId":"machine-1"},`, 100), ",") + "]"
		_, _ = io.WriteString(w, items)
	}))
	defer server.Close()

	session := &Session{
		Client: appcli.NewClient(server.URL, "acme", "token", nil, false),
		Cache:  NewCache(),
		Scope:  Scope{SiteID: "site-1"},
	}
	command := requireTUICommand(t, "health-report list")
	var runErr error
	output := captureStdout(func() {
		runErr = command.Run(session, []string{"machine-1"})
	})
	require.NoError(t, runErr)
	assert.Equal(t, int32(1), calls.Load())
	assert.NotContains(t, output, "--all")
}

func TestGeneratedCommand_ResolvesNamesAndReturnsAPIErrors(t *testing.T) {
	requestPath := make(chan string, 1)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requestPath <- r.URL.Path
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusNotFound)
		_, _ = io.WriteString(w, `{"message":"history unavailable"}`)
	}))
	defer server.Close()

	client := appcli.NewClient(server.URL, "acme", "token", nil, false)
	cache := NewCache()
	cache.Set("machine", []NamedItem{{Name: "host-one", ID: "machine-1"}})
	resolver := NewResolver(cache)
	resolver.RegisterFetcher("machine", func(context.Context) ([]NamedItem, error) {
		return nil, assert.AnError
	})
	session := &Session{Client: client, Cache: cache, Resolver: resolver}
	command := requireTUICommand(t, "machine status-history")

	err := command.Run(session, []string{"host-one"})
	require.Error(t, err)
	assert.Equal(t, "/v2/org/acme/nico/machine/machine-1/status-history", <-requestPath)
	assert.Contains(t, err.Error(), "history unavailable")
}

func TestGeneratedPathResourcePolicy_CoversEveryParameter(t *testing.T) {
	session := NewSession(
		appcli.NewClient("http://example.invalid", "acme", "token", nil, false),
		"acme",
		"",
	)
	var freeForm []string
	for _, info := range embeddedGeneratedCommandInfos {
		for _, parameter := range info.PathParameters {
			descriptor := GeneratedPathResourceDescriptor(info.Name, parameter)
			if descriptor.FreeFormReason != "" {
				freeForm = append(freeForm, info.Name+"|"+parameter)
				continue
			}
			selectorBacked := descriptor.ParentParameter != "" ||
				session.Resolver.HasFetcher(descriptor.ResourceType)
			assert.Truef(
				t,
				selectorBacked,
				"%s path parameter %s has neither a selector nor a reviewed free-form reason",
				info.Name,
				parameter,
			)
		}
	}
	assert.ElementsMatch(t, []string{
		"measured-boot machine remove|id",
		"measured-boot profile remove|id",
		"measured-boot-trusted-machine delete|id",
		"measured-boot-trusted-profile delete|id",
		"nvlink-domain firmware-update-nvlink-domain firmware-update-nvlink-domain|id",
		"nvlink-domain power-control-nvlink-domain|id",
		"task cancel|id",
		"task cancel cancel-task|id",
		"task get|id",
	}, freeForm)
}

func TestCanonicalGeneratedResourceType_NormalizesSelectorKeys(t *testing.T) {
	for name, test := range map[string]struct {
		command   string
		parameter string
		want      string
	}{
		"infiniband acronym": {
			command: "infiniband-partition delete", parameter: "infiniBandPartitionId",
			want: "infiniband-partition",
		},
		"nvlink acronym": {
			command: "nvlink-logical-partition delete", parameter: "nvLinkLogicalPartitionId",
			want: "nvlink-logical-partition",
		},
		"numbered vpc": {
			command: "vpc-peering create", parameter: "vpc1Id", want: "vpc",
		},
		"tenant account": {
			command: "tenant-account get", parameter: "accountId", want: "tenant-account",
		},
		"audit entry": {
			command: "audit get", parameter: "auditEntryId", want: "audit",
		},
		"tray component": {
			command:   "tray firmware-update-trays firmware-update-trays",
			parameter: "componentIds",
			want:      "tray-component",
		},
	} {
		t.Run(name, func(t *testing.T) {
			assert.Equal(
				t,
				test.want,
				CanonicalGeneratedResourceType(test.command, test.parameter),
			)
		})
	}
}

func TestResolveGeneratedPathParameters_UsesDependentListSurfaces(t *testing.T) {
	tests := []struct {
		name         string
		command      string
		cache        map[string][]NamedItem
		listPath     string
		listResponse string
		want         []string
	}{
		{
			name:    "allocation constraint",
			command: "allocation constraint update",
			cache: map[string][]NamedItem{
				"allocation": {{Name: "Allocation One", ID: "allocation-1"}},
			},
			listPath: "/v2/org/acme/nico/allocation/allocation-1",
			listResponse: `{
				"allocationConstraints": [{
					"id": "constraint-1",
					"resourceType": "InstanceType",
					"resourceTypeId": "instance-type-1",
					"constraintType": "Reserved",
					"instanceType": {"name": "x3.large"}
				}]
			}`,
			want: []string{"allocation-1", "constraint-1"},
		},
		{
			name:    "DPU extension service version",
			command: "dpu-extension-service version get",
			cache: map[string][]NamedItem{
				"dpu-extension-service": {{Name: "Service One", ID: "service-1"}},
			},
			listPath:     "/v2/org/acme/nico/dpu-extension-service/service-1",
			listResponse: `{"version":"v2","activeVersions":["v2"]}`,
			want:         []string{"service-1", "v2"},
		},
		{
			name:    "health report source",
			command: "health-report delete",
			cache: map[string][]NamedItem{
				"machine": {{Name: "host-one", ID: "machine-1"}},
			},
			listPath:     "/v2/org/acme/nico/machine/machine-1/health-report",
			listResponse: `[{"source":"overrides.sre","mode":"Replace"}]`,
			want:         []string{"machine-1", "overrides.sre"},
		},
		{
			name:    "instance type machine association",
			command: "instance-type machine-association delete",
			cache: map[string][]NamedItem{
				"instance-type": {{Name: "x3.large", ID: "instance-type-1"}},
				"machine":       {{Name: "host-one", ID: "machine-1"}},
			},
			listPath: "/v2/org/acme/nico/instance/type/instance-type-1/machine",
			listResponse: `[{
				"id":"deprecated-association-1",
				"machineId":"machine-1",
				"instanceTypeId":"instance-type-1"
			}]`,
			want: []string{"instance-type-1", "machine-1"},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			requestPath := make(chan string, 1)
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				requestPath <- r.URL.Path
				w.Header().Set("Content-Type", "application/json")
				_, _ = io.WriteString(w, test.listResponse)
			}))
			defer server.Close()

			session := NewSession(
				appcli.NewClient(server.URL, "acme", "token", nil, false),
				"acme",
				"",
			)
			for resourceType, items := range test.cache {
				session.Cache.Set(resourceType, items)
			}

			var (
				got []string
				err error
			)
			_ = captureStdout(func() {
				got, err = resolveGeneratedPathParameters(
					session,
					requireGeneratedInfo(t, test.command),
					nil,
				)
			})
			require.NoError(t, err)
			assert.Equal(t, test.want, got)
			assert.Equal(t, test.listPath, <-requestPath)
		})
	}
}

func TestGeneratedRuleGet_ResolvesSiteBeforeRule(t *testing.T) {
	type request struct {
		path  string
		query string
	}
	requests := make(chan request, 3)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests <- request{path: r.URL.Path, query: r.URL.RawQuery}
		w.Header().Set("Content-Type", "application/json")
		switch r.URL.Path {
		case "/v2/org/acme/nico/site":
			_, _ = io.WriteString(w, `[{"id":"site-1","name":"Site One"}]`)
		case "/v2/org/acme/nico/task/rule":
			_, _ = io.WriteString(w, `[{
				"id":"rule-1",
				"name":"Power On",
				"operationType":"PowerControl",
				"operationCode":"power_on"
			}]`)
		case "/v2/org/acme/nico/task/rule/rule-1":
			_, _ = io.WriteString(w, `{"id":"rule-1","name":"Power On"}`)
		default:
			http.NotFound(w, r)
		}
	}))
	defer server.Close()

	session := NewSession(
		appcli.NewClient(server.URL, "acme", "token", nil, false),
		"acme",
		"",
	)
	var runErr error
	_ = captureStdout(func() {
		runErr = requireTUICommand(t, "rule get").Run(session, nil)
	})
	require.NoError(t, runErr)
	assert.Equal(t, "site-1", session.Scope.SiteID)

	siteRequest := <-requests
	ruleListRequest := <-requests
	ruleGetRequest := <-requests
	assert.Equal(t, "/v2/org/acme/nico/site", siteRequest.path)
	assert.Equal(t, "/v2/org/acme/nico/task/rule", ruleListRequest.path)
	assert.Contains(t, ruleListRequest.query, "siteId=site-1")
	assert.Equal(t, "/v2/org/acme/nico/task/rule/rule-1", ruleGetRequest.path)
	assert.Contains(t, ruleGetRequest.query, "siteId=site-1")
}

func TestGeneratedRuleCreate_OpaqueJSONPreservesAuthoredSite(t *testing.T) {
	type request struct {
		path string
		body string
	}
	requests := make(chan request, 2)
	var calls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls.Add(1)
		body, _ := io.ReadAll(r.Body)
		requests <- request{path: r.URL.Path, body: string(body)}
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusCreated)
		_, _ = io.WriteString(w, `{"id":"rule-1","name":"Power On"}`)
	}))
	defer server.Close()

	session := NewSession(
		appcli.NewClient(server.URL, "acme", "token", nil, false),
		"acme",
		"",
	)
	body := `{
		"siteId":"site-from-json",
		"name":"Power On",
		"operationType":"PowerControl",
		"operationCode":"power_on",
		"ruleDefinition":{"version":"v1","steps":[]}
	}`
	_, err := withStdin(t, "y\n", func() (string, error) {
		var runErr error
		output := captureStdout(func() {
			runErr = requireTUICommand(t, "rule create").Run(
				session,
				[]string{"--data", body},
			)
		})
		return output, runErr
	})
	require.NoError(t, err)
	assert.Equal(t, int32(1), calls.Load(), "opaque JSON must not trigger a Site-list request")
	assert.Empty(t, session.Scope.SiteID)

	got := <-requests
	assert.Equal(t, "/v2/org/acme/nico/task/rule", got.path)
	assert.JSONEq(t, body, got.body)
}

func TestResolveGeneratedResource_NilResolverReturnsErrorForInteractiveDependentChoice(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(w, `[{"source":"overrides.sre","mode":"Replace"}]`)
	}))
	defer server.Close()

	session := &Session{
		Client: appcli.NewClient(server.URL, "acme", "token", nil, false),
	}
	_, supported, err := session.ResolveGeneratedResource(
		context.Background(),
		GeneratedPathResourceDescriptor("health-report delete", "source"),
		map[string]string{"machineId": "machine-1"},
		"Source",
		"",
	)
	assert.True(t, supported)
	require.Error(t, err)
	assert.Contains(t, err.Error(), "interactive resolver is required")
}

func TestResolveGeneratedPathParameters_PreservesExplicitUnsupportedID(t *testing.T) {
	session := &Session{Resolver: NewResolver(NewCache())}
	info := appcli.GeneratedCommandInfo{
		Name:           "widget get",
		PathParameters: []string{"widgetId"},
	}

	got, err := resolveGeneratedPathParameters(session, info, []string{"widget-123"})

	require.NoError(t, err)
	assert.Equal(t, []string{"widget-123"}, got)
}

func TestResolveGeneratedResource_RejectsAmbiguousNames(t *testing.T) {
	resolver := NewResolver(NewCache())
	resolver.RegisterFetcher("machine", func(context.Context) ([]NamedItem, error) {
		return []NamedItem{
			{Name: "duplicate", ID: "machine-1"},
			{Name: "duplicate", ID: "machine-2"},
		}, nil
	})
	session := &Session{Resolver: resolver}
	descriptor := GeneratedPathResourceDescriptor("machine get", "machineId")

	_, supported, err := session.ResolveGeneratedResource(
		context.Background(),
		descriptor,
		nil,
		"Machine",
		"duplicate",
	)
	assert.True(t, supported)
	require.Error(t, err)
	assert.Contains(t, err.Error(), "ambiguous")

	item, supported, err := session.ResolveGeneratedResource(
		context.Background(),
		descriptor,
		nil,
		"Machine",
		"machine-2",
	)
	require.NoError(t, err)
	assert.True(t, supported)
	assert.Equal(t, "machine-2", item.ID)
}

func TestFetchAllocationConstraints_UsesIDWhenDisplayPartsAreEmpty(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = io.WriteString(w, `{"allocationConstraints":[{"id":"constraint-1"}]}`)
	}))
	defer server.Close()
	session := &Session{Client: appcli.NewClient(server.URL, "acme", "token", nil, false)}

	items, err := session.fetchAllocationConstraints("allocation-1")

	require.NoError(t, err)
	require.Len(t, items, 1)
	assert.Equal(t, "constraint-1", items[0].Name)
}

func TestGeneratedRackAndTrayPaths_SelectSiteBeforeResource(t *testing.T) {
	for _, test := range []struct {
		command      string
		resourcePath string
		resourceBody string
		finalPath    string
	}{
		{
			command:      "rack tasks get",
			resourcePath: "/v2/org/acme/nico/rack",
			resourceBody: `[{"id":"rack-1","name":"Rack One"}]`,
			finalPath:    "/v2/org/acme/nico/rack/rack-1/task",
		},
		{
			command:      "tray tasks get",
			resourcePath: "/v2/org/acme/nico/tray",
			resourceBody: `[{"id":"tray-1","name":"Tray One","componentId":"component-1"}]`,
			finalPath:    "/v2/org/acme/nico/tray/tray-1/task",
		},
	} {
		t.Run(test.command, func(t *testing.T) {
			requests := make(chan string, 3)
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				requests <- r.URL.Path
				w.Header().Set("Content-Type", "application/json")
				switch r.URL.Path {
				case "/v2/org/acme/nico/site":
					_, _ = io.WriteString(w, `[{"id":"site-1","name":"Site One"}]`)
				case test.resourcePath:
					_, _ = io.WriteString(w, test.resourceBody)
				case test.finalPath:
					_, _ = io.WriteString(w, `[]`)
				default:
					http.NotFound(w, r)
				}
			}))
			defer server.Close()

			session := NewSession(
				appcli.NewClient(server.URL, "acme", "token", nil, false),
				"acme",
				"",
			)
			var runErr error
			_ = captureStdout(func() {
				runErr = requireTUICommand(t, test.command).Run(session, nil)
			})
			require.NoError(t, runErr)
			assert.Equal(t, "site-1", session.Scope.SiteID)
			assert.Equal(t, "/v2/org/acme/nico/site", <-requests)
			assert.Equal(t, test.resourcePath, <-requests)
			assert.Equal(t, test.finalPath, <-requests)
		})
	}
}

func TestGeneratedOptionalQueryFiltersAreNotForced(t *testing.T) {
	for _, test := range []struct {
		name     string
		args     []string
		wantType string
	}{
		{name: "required site only"},
		{name: "explicit optional filter", args: []string{"--type", "compute"}, wantType: "compute"},
	} {
		t.Run(test.name, func(t *testing.T) {
			requests := make(chan *http.Request, 2)
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				requests <- r.Clone(r.Context())
				w.Header().Set("Content-Type", "application/json")
				switch r.URL.Path {
				case "/v2/org/acme/nico/site":
					_, _ = io.WriteString(w, `[{"id":"site-1","name":"Site One"}]`)
				case "/v2/org/acme/nico/tray/validation":
					_, _ = io.WriteString(w, `{"valid":true}`)
				default:
					http.NotFound(w, r)
				}
			}))
			defer server.Close()

			session := NewSession(
				appcli.NewClient(server.URL, "acme", "token", nil, false),
				"acme",
				"",
			)
			var runErr error
			_ = captureStdout(func() {
				runErr = requireTUICommand(
					t,
					"tray validate-trays validate-trays",
				).Run(session, test.args)
			})
			require.NoError(t, runErr)

			siteRequest := <-requests
			assert.Equal(t, "/v2/org/acme/nico/site", siteRequest.URL.Path)
			validationRequest := <-requests
			assert.Equal(t, "/v2/org/acme/nico/tray/validation", validationRequest.URL.Path)
			assert.Equal(t, "site-1", validationRequest.URL.Query().Get("siteId"))
			assert.Equal(t, test.wantType, validationRequest.URL.Query().Get("type"))
			for _, optional := range []string{
				"componentId", "manufacturer", "name", "rackId", "rackName", "slotId",
			} {
				assert.Empty(t, validationRequest.URL.Query().Get(optional), optional)
			}
		})
	}
}

func TestSplitCommandArguments_PreservesQuotedJSON(t *testing.T) {
	got, err := splitCommandArguments(`--data '{"name": "two words"}' machine-1`)
	require.NoError(t, err)
	assert.Equal(t, []string{"--data", `{"name": "two words"}`, "machine-1"}, got)
}

func TestSplitCommandArguments_RejectsUnterminatedQuote(t *testing.T) {
	_, err := splitCommandArguments(`--data '{"name":"broken"}`)
	require.Error(t, err)
	assert.Contains(t, err.Error(), "unterminated")
}

func TestMergeGeneratedDataFieldPreservesJSONNumberLiterals(t *testing.T) {
	info := appcli.GeneratedCommandInfo{
		Flags: []appcli.GeneratedCommandFlag{{Name: "data", TakesValue: true}},
	}

	got, err := mergeGeneratedDataField(
		info,
		[]string{"--data", `{"count":9007199254740993}`},
		"siteId",
		"site-1",
	)

	require.NoError(t, err)
	require.Len(t, got, 2)
	assert.Contains(t, got[1], `"count":9007199254740993`)
	assert.Contains(t, got[1], `"siteId":"site-1"`)
}

func TestMergeGeneratedDataFieldRejectsTrailingJSON(t *testing.T) {
	info := appcli.GeneratedCommandInfo{
		Flags: []appcli.GeneratedCommandFlag{{Name: "data", TakesValue: true}},
	}

	_, err := mergeGeneratedDataField(
		info,
		[]string{"--data", `{"count":1}{"count":2}`},
		"siteId",
		"site-1",
	)

	require.Error(t, err)
	assert.Contains(t, err.Error(), "request body is not JSON")
}

func TestLogGeneratedCommandQuotesShellUnsafeArguments(t *testing.T) {
	output := captureStdout(func() {
		logGeneratedCommand(
			&Session{},
			appcli.GeneratedCommandInfo{Name: "machine get"},
			[]string{"plain", "$HOME;touch", "O'Brien"},
		)
	})

	assert.Contains(t, output, "machine get plain '$HOME;touch' 'O'\"'\"'Brien'")
}

func TestValidateGeneratedBodyArguments_RejectsCompetingInputs(t *testing.T) {
	info := appcli.GeneratedCommandInfo{
		Flags: []appcli.GeneratedCommandFlag{
			{Name: "data", TakesValue: true},
			{Name: "password", TakesValue: true},
		},
		BodyFields: []appcli.GeneratedCommandBodyField{{
			JSONName: "password",
			FlagName: "password",
		}},
	}
	err := validateGeneratedBodyArguments(info, []string{
		"--data", `{"password":"secret"}`,
		"--password", "other-secret",
	})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "--data cannot be combined")
}

func TestMatchCommandLine_UsesLongestBoundaryMatch(t *testing.T) {
	commands := []Command{
		{Name: "rule list", Description: "short"},
		{Name: "rule list-rules list-rules", Description: "long"},
	}
	commandMap := map[string]Command{}
	for _, command := range commands {
		commandMap[command.Name] = command
	}

	command, rest, ok := matchCommandLine("rule list-rules list-rules --all", commandMap, commands)
	require.True(t, ok)
	assert.Equal(t, "rule list-rules list-rules", command.Name)
	assert.Equal(t, "--all", rest)
}

func commandNames(commands []Command) map[string]struct{} {
	names := make(map[string]struct{}, len(commands))
	for _, command := range commands {
		names[command.Name] = struct{}{}
	}
	return names
}

func requireTUICommand(t *testing.T, name string) Command {
	t.Helper()
	for _, command := range AllCommands() {
		if command.Name == name {
			return command
		}
	}
	t.Fatalf("TUI command %q not found", name)
	return Command{}
}

func requireGeneratedInfo(t *testing.T, name string) appcli.GeneratedCommandInfo {
	t.Helper()
	for _, info := range embeddedGeneratedCommandInfos {
		if info.Name == name {
			return info
		}
	}
	t.Fatalf("generated command info %q not found", name)
	return appcli.GeneratedCommandInfo{}
}
