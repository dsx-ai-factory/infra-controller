// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"context"
	"io"
	"net/http"
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestExpectedMachineInternalServerErrorDecoding(t *testing.T) {
	const message = "Failed to validate SKU ID in request data due to DB error"
	const body = `{"source":"nico","message":"Failed to validate SKU ID in request data due to DB error","data":null}`
	skuID := "test-sku"
	tests := []struct {
		name    string
		execute func(*APIClient) (*ExpectedMachine, *http.Response, error)
	}{
		{
			name: "POST",
			execute: func(client *APIClient) (*ExpectedMachine, *http.Response, error) {
				return client.ExpectedMachineAPI.CreateExpectedMachine(context.Background(), "test-org").
					ExpectedMachineCreateRequest(ExpectedMachineCreateRequest{
						SiteId:              "f97df110-f4de-492e-8849-4a6af68026b0",
						BmcMacAddress:       "00:11:22:33:44:55",
						ChassisSerialNumber: "test-machine",
						SkuId:               *NewNullableString(&skuID),
					}).Execute()
			},
		},
		{
			name: "PATCH",
			execute: func(client *APIClient) (*ExpectedMachine, *http.Response, error) {
				return client.ExpectedMachineAPI.UpdateExpectedMachine(context.Background(), "test-org", "6353e613-e4c1-485e-8311-eeb07077735f").
					ExpectedMachineUpdateRequest(ExpectedMachineUpdateRequest{
						SkuId: *NewNullableString(&skuID),
					}).Execute()
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cfg := NewConfiguration()
			cfg.Servers = ServerConfigurations{{URL: "https://example.com", Description: "test"}}
			cfg.HTTPClient = &http.Client{Transport: expectedMachineErrorTransport{body: body}}

			result, response, err := tt.execute(NewAPIClient(cfg))

			require.Nil(t, result)
			require.NotNil(t, response)
			assert.Equal(t, http.StatusInternalServerError, response.StatusCode)
			var apiError *GenericOpenAPIError
			require.ErrorAs(t, err, &apiError)
			assert.Equal(t, body, string(apiError.Body()))
			model, ok := apiError.Model().(NICoAPIError)
			require.True(t, ok, "500 response must decode as NICoAPIError")
			assert.Equal(t, "nico", model.GetSource())
			assert.Equal(t, message, model.GetMessage())
			assert.Nil(t, model.Data)
			assert.Nil(t, model.Retryable)
		})
	}
}

type expectedMachineErrorTransport struct {
	body string
}

func (transport expectedMachineErrorTransport) RoundTrip(request *http.Request) (*http.Response, error) {
	return &http.Response{
		StatusCode: http.StatusInternalServerError,
		Status:     "500 Internal Server Error",
		Header:     http.Header{"Content-Type": []string{"application/json"}},
		Body:       io.NopCloser(strings.NewReader(transport.body)),
		Request:    request,
	}, nil
}
