// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"

	appcli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestSession_fetchAll(t *testing.T) {
	firstPage := make([]map[string]string, 100)
	for i := range firstPage {
		firstPage[i] = map[string]string{"id": fmt.Sprintf("site-%d", i)}
	}
	firstBody, err := json.Marshal(firstPage)
	require.NoError(t, err)

	for _, test := range []struct {
		name          string
		pages         []string
		wantError     string
		wantErrorType any
	}{
		{
			name:          "malformed first page",
			pages:         []string{"["},
			wantError:     "parsing /v2/org/{org}/nico/site page 1:",
			wantErrorType: new(*json.SyntaxError),
		},
		{
			name:          "invalid later page",
			pages:         []string{string(firstBody), `{}`},
			wantError:     "parsing /v2/org/{org}/nico/site page 2:",
			wantErrorType: new(*json.UnmarshalTypeError),
		},
		{
			name:  "empty list",
			pages: []string{`[]`},
		},
	} {
		t.Run(test.name, func(t *testing.T) {
			requests := 0
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				requests++
				assert.Equal(t, "/v2/org/acme/nico/site", r.URL.Path)
				assert.Equal(t, fmt.Sprint(requests), r.URL.Query().Get("pageNumber"))
				assert.Equal(t, "100", r.URL.Query().Get("pageSize"))
				if !assert.LessOrEqual(t, requests, len(test.pages)) {
					http.Error(w, "unexpected page", http.StatusInternalServerError)
					return
				}
				_, err := io.WriteString(w, test.pages[requests-1])
				assert.NoError(t, err)
			}))
			defer server.Close()
			session := NewSession(appcli.NewClient(server.URL, "acme", "token", nil, false), "acme", "")

			items, err := session.fetchAll(apiPath(session, "site"), nil)

			assert.Equal(t, len(test.pages), requests)
			if test.wantError != "" {
				require.ErrorContains(t, err, test.wantError)
				assert.Nil(t, items)
				assert.ErrorAs(t, err, test.wantErrorType)
				return
			}
			require.NoError(t, err)
			assert.Empty(t, items)
		})
	}
}
