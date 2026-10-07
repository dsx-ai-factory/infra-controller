// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	appcli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestQuoteCommandArgumentRoundTripsEscapedCharacters(t *testing.T) {
	for _, value := range []string{
		"two words",
		"tab\tseparated",
		"line\nseparated",
		"control\x01byte",
		`quote"and\backslash`,
	} {
		t.Run(strings.ReplaceAll(value, "\n", `\n`), func(t *testing.T) {
			args, err := splitCommandArguments(quoteCommandArgument(value))
			require.NoError(t, err)
			assert.Equal(t, []string{value}, args)
		})
	}
}

func TestGeneratedResourceSuggestionsBoundFetcherContext(t *testing.T) {
	resolver := NewResolver(NewCache())
	var remaining time.Duration
	resolver.RegisterFetcher("machine", func(ctx context.Context) ([]NamedItem, error) {
		deadline, ok := ctx.Deadline()
		require.True(t, ok)
		remaining = time.Until(deadline)
		return nil, context.DeadlineExceeded
	})
	session := &Session{Resolver: resolver}

	got := getGeneratedResourceSuggestions(
		session,
		appcli.GeneratedCommandInfo{
			Name:           "machine inspect",
			PathParameters: []string{"machineId"},
		},
		"",
	)

	assert.Nil(t, got)
	assert.Greater(t, remaining, time.Duration(0))
	assert.LessOrEqual(t, remaining, 2*time.Second)
}

func TestMatchGeneratedAutocompleteItemRejectsAmbiguousNames(t *testing.T) {
	items := []NamedItem{
		{Name: "duplicate", ID: "machine-1"},
		{Name: "duplicate", ID: "machine-2"},
	}

	_, found := matchGeneratedAutocompleteItem(items, "duplicate")
	assert.False(t, found)

	item, found := matchGeneratedAutocompleteItem(items, "machine-2")
	assert.True(t, found)
	assert.Equal(t, "machine-2", item.ID)
}

func TestGetAllSuggestions_GeneratedResourcesUseCanonicalFetchers(t *testing.T) {
	tests := []struct {
		name         string
		command      string
		input        string
		resourceType string
		item         NamedItem
	}{
		{
			name:         "InfiniBand acronym",
			command:      "infiniband-partition delete",
			input:        "infiniband-partition delete fab",
			resourceType: "infiniband-partition",
			item:         NamedItem{Name: "fabric-a", ID: "ib-1"},
		},
		{
			name:         "NVLink acronym",
			command:      "nvlink-logical-partition update",
			input:        "nvlink-logical-partition update train",
			resourceType: "nvlink-logical-partition",
			item:         NamedItem{Name: "training-fabric", ID: "nvlink-1"},
		},
		{
			name:         "newly registered iPXE resource",
			command:      "ipxe-template get",
			input:        "ipxe-template get boot",
			resourceType: "ipxe-template",
			item:         NamedItem{Name: "boot-template", ID: "ipxe-1"},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			cache := NewCache()
			resolver := NewResolver(cache)
			resolver.RegisterFetcher(
				test.resourceType,
				func(context.Context) ([]NamedItem, error) {
					return []NamedItem{test.item}, nil
				},
			)
			session := &Session{Cache: cache, Resolver: resolver}

			suggestions := getAllSuggestions(
				session,
				test.input,
				[]string{test.command},
			)

			assert.Equal(t, []string{test.command + " " + test.item.Name}, suggestions)
		})
	}
}

func TestGetAllSuggestions_GeneratedDependentSecondPathUsesResolvedParent(t *testing.T) {
	requestPath := make(chan string, 1)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requestPath <- r.URL.Path
		w.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(w, `{
			"allocationConstraints": [{
				"id": "constraint-1",
				"resourceType": "InstanceType",
				"resourceTypeId": "instance-type-1",
				"instanceType": {"name": "gpu.large"},
				"constraintType": "ExactMatch"
			}]
		}`)
	}))
	defer server.Close()

	client := appcli.NewClient(server.URL, "acme", "token", nil, false)
	session := NewSession(client, "acme", "")
	session.Resolver.RegisterFetcher(
		"allocation",
		func(context.Context) ([]NamedItem, error) {
			return []NamedItem{{Name: "workload-a", ID: "allocation-1"}}, nil
		},
	)

	suggestions := getAllSuggestions(
		session,
		"allocation constraint update workload-a gpu",
		[]string{"allocation constraint update"},
	)

	require.Equal(
		t,
		[]string{`allocation constraint update workload-a "InstanceType / gpu.large"`},
		suggestions,
	)
	assert.Equal(
		t,
		"/v2/org/acme/nico/allocation/allocation-1",
		<-requestPath,
	)
	args, err := splitCommandArguments(
		suggestions[0][len("allocation constraint update "):],
	)
	require.NoError(t, err)
	assert.Equal(t, []string{"workload-a", "InstanceType / gpu.large"}, args)
}

func TestGetAllSuggestions_GeneratedAssociationUsesParentScopedMachineNames(t *testing.T) {
	requestPath := make(chan string, 1)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requestPath <- r.URL.Path
		w.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(
			w,
			`[{"id":"association-1","machineId":"machine-1"}]`,
		)
	}))
	defer server.Close()

	client := appcli.NewClient(server.URL, "acme", "token", nil, false)
	session := NewSession(client, "acme", "")
	session.Resolver.RegisterFetcher(
		"instance-type",
		func(context.Context) ([]NamedItem, error) {
			return []NamedItem{{Name: "gpu.large", ID: "instance-type-1"}}, nil
		},
	)
	session.Resolver.RegisterFetcher(
		"machine",
		func(context.Context) ([]NamedItem, error) {
			return []NamedItem{{Name: "host-one", ID: "machine-1"}}, nil
		},
	)

	suggestions := getAllSuggestions(
		session,
		"instance-type machine-association delete gpu.large host",
		[]string{"instance-type machine-association delete"},
	)

	assert.Equal(
		t,
		[]string{"instance-type machine-association delete gpu.large host-one"},
		suggestions,
	)
	assert.Equal(
		t,
		"/v2/org/acme/nico/instance/type/instance-type-1/machine",
		<-requestPath,
	)
}

func TestGetAllSuggestions_PreservesManualAliasCompletion(t *testing.T) {
	cache := NewCache()
	resolver := NewResolver(cache)
	resolver.RegisterFetcher("machine", func(context.Context) ([]NamedItem, error) {
		return []NamedItem{{Name: "host-one", ID: "machine-1"}}, nil
	})
	session := &Session{Cache: cache, Resolver: resolver}

	suggestions := getAllSuggestions(
		session,
		"machine dpu get host",
		[]string{"machine dpu get"},
	)

	assert.Equal(t, []string{"machine dpu get host-one"}, suggestions)
}

func TestGetAllSuggestions_GeneratedResourceAfterValueFlag(t *testing.T) {
	cache := NewCache()
	resolver := NewResolver(cache)
	resolver.RegisterFetcher("machine", func(context.Context) ([]NamedItem, error) {
		return []NamedItem{{Name: "host-one", ID: "machine-1"}}, nil
	})
	session := &Session{Cache: cache, Resolver: resolver}

	for _, test := range []struct {
		name  string
		input string
		want  []string
	}{
		{
			name:  "separate flag value before resource",
			input: "machine status-history --page-size 10 host",
			want:  []string{"machine status-history --page-size 10 host-one"},
		},
		{
			name:  "inline flag value before resource",
			input: "machine status-history --page-size=10 host",
			want:  []string{"machine status-history --page-size=10 host-one"},
		},
		{
			name:  "completed flag value offers all resources",
			input: "machine status-history --page-size 10 ",
			want:  []string{"machine status-history --page-size 10 host-one"},
		},
		{
			name:  "incomplete flag value is not a resource filter",
			input: "machine status-history --page-size ",
		},
		{
			name:  "unknown flag suppresses resource completion",
			input: "machine status-history --unknown value host",
		},
		{
			name:  "flag after positional is invalid",
			input: "machine status-history host-one --page-size 10 ",
		},
	} {
		t.Run(test.name, func(t *testing.T) {
			assert.Equal(
				t,
				test.want,
				getAllSuggestions(
					session,
					test.input,
					[]string{"machine status-history"},
				),
			)
		})
	}
}
