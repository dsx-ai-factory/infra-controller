// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"net/http"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard/helpers"
)

// NewConfigurationWithAPIName returns a generated configuration with an
// additional API path override for deployments that use a non-default API name.
func NewConfigurationWithAPIName(apiName string) *Configuration {
	cfg := NewConfiguration()
	cfg.SetAPIName(apiName)
	return cfg
}

// SetAPIName configures an HTTP transport that rewrites the API path segment
// after /org/{org}/ before a request is sent. If you also provide a custom
// HTTP client, call SetAPIName after assigning that client to the configuration.
func (c *Configuration) SetAPIName(apiName string) {
	if rewriter, ok := helpers.CurrentAPINameRewriteTransport(c.HTTPClient); ok {
		rewriter.SetAPIName(apiName)
		return
	}

	baseClient := c.HTTPClient
	if baseClient == nil {
		baseClient = http.DefaultClient
	}

	clientCopy := *baseClient
	clientCopy.Transport = helpers.NewAPINameRewriteTransport(apiName, baseClient.Transport)
	c.HTTPClient = &clientCopy
}

// GetAPIName returns the configured API path segment. When unset, nico is
// used to match the OpenAPI-generated paths.
func (c *Configuration) GetAPIName() string {
	if rewriter, ok := helpers.CurrentAPINameRewriteTransport(c.HTTPClient); ok {
		return rewriter.APIName()
	}
	return helpers.DefaultAPIName
}
