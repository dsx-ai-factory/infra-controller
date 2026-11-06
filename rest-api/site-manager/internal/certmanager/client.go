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
	"net/url"
	"os"
	"time"

	commontls "github.com/NVIDIA/infra-controller/rest-api/common/pkg/tls"
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

// Options configures the verified endpoint and optional client identity.
type Options struct {
	URL            string
	CAPath         string
	ClientCertPath string
	ClientKeyPath  string
}

// NewClient returns a verified Cert Manager client. A configured identity uses
// the shared renewable TLS configuration; cancellation releases its resources.
func NewClient(ctx context.Context, o Options) (*Client, error) {
	endpoint, err := url.Parse(o.URL)
	if err != nil {
		return nil, errors.Wrap(err, "parse certificate manager URL")
	}
	if endpoint.Scheme != "https" || endpoint.Host == "" || endpoint.User != nil {
		return nil, fmt.Errorf("certificate manager URL must use HTTPS without user information")
	}
	if o.CAPath == "" {
		return nil, fmt.Errorf("a CA bundle path is required to verify the Cert Manager endpoint")
	}
	caPEM, err := os.ReadFile(o.CAPath)
	if err != nil {
		return nil, errors.Wrapf(err, "read Cert Manager CA bundle %q", o.CAPath)
	}
	pool := x509.NewCertPool()
	if !pool.AppendCertsFromPEM(caPEM) {
		return nil, fmt.Errorf("the Cert Manager CA bundle %q contains no certificates", o.CAPath)
	}
	tlsConfig := &tls.Config{RootCAs: pool, MinVersion: tls.VersionTLS12}
	if o.ClientCertPath != "" || o.ClientKeyPath != "" {
		if o.ClientCertPath == "" || o.ClientKeyPath == "" {
			return nil, fmt.Errorf("both client certificate and key paths are required")
		}
		dynamic, err := commontls.NewDynTLSCfg(o.ClientKeyPath, o.ClientCertPath, o.CAPath)
		if err != nil {
			return nil, errors.Wrap(err, "configure certificate manager client identity")
		}
		context.AfterFunc(ctx, dynamic.Close)
		tlsConfig = dynamic.ClientCfg()
	}
	client := &http.Client{
		Timeout: requestTimeout,
		Transport: otelhttp.NewTransport(&http.Transport{TLSClientConfig: tlsConfig},
			otelhttp.WithSpanNameFormatter(func(_ string, _ *http.Request) string { return spanName })),
		CheckRedirect: func(_ *http.Request, _ []*http.Request) error { return http.ErrUseLastResponse },
	}
	context.AfterFunc(ctx, client.CloseIdleConnections)
	return &Client{url: o.URL, httpClient: client}, nil
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
