// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestRackAPIService_PowerControlRackExecute(t *testing.T) {
	tests := []struct {
		name string
		code int
		body string
	}{
		{name: "task conflict decodes as API error", code: http.StatusPreconditionFailed, body: `{"source":"nico","message":"rack has a conflicting task","data":null}`},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				assert.Equal(t, http.MethodPatch, r.Method)
				w.Header().Set("Content-Type", "application/json")
				w.WriteHeader(tt.code)
				_, err := w.Write([]byte(tt.body))
				assert.NoError(t, err)
			}))
			defer server.Close()
			cfg := NewConfiguration()
			cfg.Servers = ServerConfigurations{{URL: server.URL}}
			client := NewAPIClient(cfg)
			result, response, err := client.RackAPI.PowerControlRack(context.Background(), "test-org", "4bfe3a12-a390-4ec9-a860-0da9f1bfab42").UpdatePowerStateRequest(UpdatePowerStateRequest{SiteId: "7b8d00d5-fd66-4914-ae12-21c916bbd309", State: "On"}).Execute()
			require.Nil(t, result)
			require.NotNil(t, response)
			require.Equal(t, tt.code, response.StatusCode)
			var apiError *GenericOpenAPIError
			require.ErrorAs(t, err, &apiError)
			model, ok := apiError.Model().(NICoAPIError)
			require.True(t, ok)
			require.Equal(t, "nico", model.GetSource())
			require.Equal(t, "rack has a conflicting task", model.GetMessage())
			require.Nil(t, model.Data)
		})
	}
}
