// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package matclient provides an HTTP client for the machine-a-tron API.
package matclient

import (
	"context"
	"crypto/tls"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"time"

	"github.com/rs/zerolog"
)

const (
	// maxResponseSize is the maximum allowed response body size (10 MB).
	maxResponseSize = 10 * 1024 * 1024
	// maxErrorBodySize is the maximum error body size to include in error messages.
	maxErrorBodySize = 1024
)

// Client is an HTTP client for the machine-a-tron API.
type Client struct {
	baseURL            string
	httpClient         *http.Client
	logger             zerolog.Logger
	insecureSkipVerify bool
}

// Option configures a Client.
type Option func(*Client)

// WithHTTPClient sets a custom HTTP client.
func WithHTTPClient(c *http.Client) Option {
	return func(client *Client) {
		client.httpClient = c
	}
}

// WithLogger sets the logger.
func WithLogger(logger zerolog.Logger) Option {
	return func(client *Client) {
		client.logger = logger
	}
}

// WithInsecureSkipVerify disables TLS certificate verification.
// Use only for development with self-signed certificates.
func WithInsecureSkipVerify() Option {
	return func(client *Client) {
		client.insecureSkipVerify = true
	}
}

// NewClient creates a new machine-a-tron API client.
// baseURL must be an absolute HTTP(S) URL with a host.
func NewClient(baseURL string, opts ...Option) (*Client, error) {
	parsed, err := url.Parse(baseURL)
	if err != nil {
		return nil, fmt.Errorf("invalid base URL: %w", err)
	}

	// Validate URL is absolute with proper scheme and host
	if parsed.Scheme != "http" && parsed.Scheme != "https" {
		return nil, fmt.Errorf("invalid base URL: scheme must be http or https, got %q", parsed.Scheme)
	}
	if parsed.Host == "" {
		return nil, fmt.Errorf("invalid base URL: missing host")
	}
	if parsed.User != nil {
		return nil, fmt.Errorf("invalid base URL: userinfo not allowed")
	}
	if parsed.RawQuery != "" {
		return nil, fmt.Errorf("invalid base URL: query string not allowed")
	}
	if parsed.Fragment != "" {
		return nil, fmt.Errorf("invalid base URL: fragment not allowed")
	}

	// Normalize: rebuild from parsed components, trim trailing slash
	parsed.Path = strings.TrimSuffix(parsed.Path, "/")
	normalizedURL := parsed.String()

	c := &Client{
		baseURL: normalizedURL,
		httpClient: &http.Client{
			Timeout: 30 * time.Second,
		},
		logger: zerolog.Nop(),
	}

	for _, opt := range opts {
		opt(c)
	}

	// Apply insecure TLS after all options to preserve existing transport settings
	if c.insecureSkipVerify {
		c.applyInsecureTLS()
	}

	return c, nil
}

// applyInsecureTLS configures the client to skip TLS verification while preserving
// existing transport settings.
func (c *Client) applyInsecureTLS() {
	transport, ok := c.httpClient.Transport.(*http.Transport)
	if !ok || transport == nil {
		transport = http.DefaultTransport.(*http.Transport).Clone()
	} else {
		transport = transport.Clone()
	}

	if transport.TLSClientConfig == nil {
		transport.TLSClientConfig = &tls.Config{}
	}
	transport.TLSClientConfig.InsecureSkipVerify = true //nolint:gosec // Intentional for dev/test with self-signed certs
	transport.TLSClientConfig.MinVersion = tls.VersionTLS12

	c.httpClient.Transport = transport
}

// Close releases resources associated with the client.
// It closes idle connections on the underlying HTTP transport.
func (c *Client) Close() error {
	c.httpClient.CloseIdleConnections()
	return nil
}

// GetMachinesStatus fetches the current machine status from machine-a-tron.
func (c *Client) GetMachinesStatus(ctx context.Context) (*MachinesStatusResponse, error) {
	reqURL := c.baseURL + "/machines/status"

	req, err := http.NewRequestWithContext(ctx, http.MethodGet, reqURL, nil)
	if err != nil {
		return nil, fmt.Errorf("creating request: %w", err)
	}

	req.Header.Set("Accept", "application/json")

	c.logger.Debug().Str("url", reqURL).Msg("fetching machine status")

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("executing request: %w", err)
	}
	defer func() { _ = resp.Body.Close() }()

	if resp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(io.LimitReader(resp.Body, maxErrorBodySize))
		return nil, fmt.Errorf("unexpected status %d: %s", resp.StatusCode, truncateString(string(body), maxErrorBodySize))
	}

	// Limit response body size to prevent memory exhaustion.
	body, err := io.ReadAll(io.LimitReader(resp.Body, maxResponseSize+1))
	if err != nil {
		return nil, fmt.Errorf("reading response: %w", err)
	}
	if len(body) > maxResponseSize {
		return nil, fmt.Errorf("response exceeds maximum size of %d bytes", maxResponseSize)
	}

	var result MachinesStatusResponse
	if err := json.Unmarshal(body, &result); err != nil {
		return nil, fmt.Errorf("decoding response: %w", err)
	}

	c.logger.Debug().Int("machine_count", len(result.Machines)).Msg("fetched machine status")

	return &result, nil
}

// truncateString truncates a string to maxLen and adds "..." if truncated.
func truncateString(s string, maxLen int) string {
	if len(s) <= maxLen {
		return s
	}
	return s[:maxLen-3] + "..."
}
