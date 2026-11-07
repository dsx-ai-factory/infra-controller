// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package certmanager is Site Manager's client for nico-rest-cert-manager.
package certmanager

import (
	"bytes"
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"time"

	"github.com/pkg/errors"
	"go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp"
)

const (
	// requestTimeout limits each request to Cert Manager, including reading
	// the response body.
	requestTimeout = 20 * time.Second
	// spanName is the span name of every Cert Manager request.
	spanName = "cert-manager-client"
)

// CertificateRequest is the body of Cert Manager's POST /v1/pki/cloud-cert.
// Cert Manager builds the certificate's DNS name from Name and App. TTL is the
// validity in hours.
type CertificateRequest struct {
	Name string `json:"name,omitempty"`
	App  string `json:"app,omitempty"`
	TTL  int    `json:"ttl,omitempty"`
}

// Certificate is Cert Manager's reply to a CertificateRequest. It holds the
// PEM private key and the PEM certificate issued for it.
type Certificate struct {
	Key         string `json:"key,omitempty"`
	Certificate string `json:"certificate,omitempty"`
}

// Client sends requests to Cert Manager at a base URL.
type Client struct {
	url        string
	httpClient *http.Client
}

// NewClient returns a client for Cert Manager at url. The CA bundle at caPath
// is the only trust anchor. So an unreadable or empty bundle fails here
// instead of falling back to an unverified connection.
func NewClient(url, caPath string) (*Client, error) {
	if caPath == "" {
		return nil, fmt.Errorf("a CA bundle path is required to verify the Cert Manager endpoint")
	}

	caPEM, err := os.ReadFile(caPath)
	if err != nil {
		return nil, errors.Wrapf(err, "read Cert Manager CA bundle %q", caPath)
	}

	pool := x509.NewCertPool()
	if !pool.AppendCertsFromPEM(caPEM) {
		return nil, fmt.Errorf("the Cert Manager CA bundle %q contains no certificates", caPath)
	}

	return &Client{
		url: url,
		httpClient: &http.Client{
			Timeout: requestTimeout,
			Transport: otelhttp.NewTransport(&http.Transport{
				TLSClientConfig: &tls.Config{
					RootCAs:    pool,
					MinVersion: tls.VersionTLS12,
				},
			}, otelhttp.WithSpanNameFormatter(func(_ string, _ *http.Request) string {
				return spanName
			})),
		},
	}, nil
}

// IssueCertificate asks Cert Manager to issue a certificate for name and app,
// valid for ttl hours.
func (c *Client) IssueCertificate(ctx context.Context, name, app string, ttl int) (*Certificate, error) {
	body := &CertificateRequest{
		Name: name,
		App:  app,
		TTL:  ttl,
	}

	payloadBuf := new(bytes.Buffer)
	err := json.NewEncoder(payloadBuf).Encode(body)
	if err != nil {
		return nil, errors.Wrap(err, "json encode payload")
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, c.url+"/v1/pki/cloud-cert", payloadBuf)
	if err != nil {
		return nil, errors.Wrap(err, "http.NewRequestWithContext")
	}
	content, err := c.request(req)
	if err != nil {
		return nil, errors.Wrap(err, "c.request(req)")
	}

	if content == nil {
		return nil, fmt.Errorf("empty response")
	}

	certificate := &Certificate{}
	err = json.Unmarshal(content, certificate)
	if err != nil {
		return nil, errors.Wrap(err, "json.Unmarshal")
	}
	return certificate, nil
}

// GetCA returns Cert Manager's CA certificate file in PEM, as served.
func (c *Client) GetCA(ctx context.Context) (string, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, c.url+"/v1/pki/ca/pem", nil)
	if err != nil {
		return "", errors.Wrap(err, "http.NewRequestWithContext")
	}
	content, err := c.request(req)
	if err != nil {
		return "", errors.Wrap(err, "c.request(req)")
	}

	if content == nil {
		return "", fmt.Errorf("empty response")
	}

	return string(content), nil
}

// request sends req and returns the response body. A status outside 200 to
// 226 is an error with the status and body. `204` returns nil.
func (c *Client) request(req *http.Request) ([]byte, error) {
	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode < http.StatusOK || resp.StatusCode > http.StatusIMUsed {
		content, _ := io.ReadAll(resp.Body)
		return nil, fmt.Errorf("%s, %s", resp.Status, content)
	}

	if resp.StatusCode == http.StatusNoContent {
		return nil, nil
	}

	return io.ReadAll(resp.Body)
}
