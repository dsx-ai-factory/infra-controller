// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package forge

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/status"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/inventory"
)

const (
	// DefaultCertDir is where the chart mounts the nico-api client cert, as MAT.
	DefaultCertDir = "/var/run/secrets/spiffe.io"
	// tlsOverride turns off nico-api server verification; writes must go over an authenticated channel, so the client refuses it when it is set.
	tlsOverride = "DISABLE_TLS_ENFORCEMENT"
	// rpcTimeout bounds connecting and the reply headers of one RPC; the body is bounded by the caller's attempt timeout.
	rpcTimeout = 60 * time.Second
)

// Settings is the nico-api client configuration.
type Settings struct {
	CertDir string
	Spiffe  SpiffeSettings
	// ServerName is the TLS server name nico-api's certificate is verified against. OPEN (spec-E section 11): the Rust default was not traced; set from config, never
	// guessed here.
	ServerName string
}

func (s Settings) certPath() string { return filepath.Join(s.dir(), "tls.crt") }
func (s Settings) keyPath() string  { return filepath.Join(s.dir(), "tls.key") }
func (s Settings) caPath() string   { return filepath.Join(s.dir(), "ca.crt") }
func (s Settings) dir() string {
	if s.CertDir == "" {
		return DefaultCertDir
	}
	return s.CertDir
}

// Client is the nico-api client over gRPC with the mounted client certificate. A failed call keeps its gRPC status for the caller's code; this client never turns a
// status into a message, which may hold anything.
type Client struct {
	conn    *grpc.ClientConn
	forge   corev1.ForgeClient
	timeout time.Duration
}

// Dial builds a nico-api client that presents the mounted client certificate over verified TLS. It refuses a URL the records reader refuses, the override that turns
// verification off, and a key that does not load or match. The pair is re-read for each connection, which fails rather than connecting without a certificate when the
// pair no longer loads: nico-api accepts a TLS client with no certificate, so an unloadable key must never send writes anonymously.
func Dial(apiURL string, s Settings) (*Client, error) {
	if reason := inventory.CheckedAPIURL(apiURL); reason != "" {
		return nil, errors.New(reason)
	}
	if _, set := os.LookupEnv(tlsOverride); set {
		return nil, errors.New(tlsOverride + " is set: scale tests talk to nico-api only over verified TLS")
	}
	if err := CheckKeyPairFiles(s.certPath(), s.keyPath()); err != nil {
		return nil, err
	}
	creds, err := transportCreds(s)
	if err != nil {
		return nil, err
	}
	u, _ := url.Parse(apiURL)
	// WithDisableRetry keeps retry classification in the writer (C); safe transparent retries stay.
	conn, err := grpc.NewClient(u.Host, grpc.WithTransportCredentials(creds), grpc.WithDisableRetry())
	if err != nil {
		return nil, errors.New("the nico-api client did not build")
	}
	return &Client{conn, corev1.NewForgeClient(conn), rpcTimeout}, nil
}

// transportCreds verifies the server against ca.crt and reloads the client pair on every handshake; a reload error aborts the handshake, so nothing is sent without a
// certificate.
func transportCreds(s Settings) (credentials.TransportCredentials, error) {
	caPEM, err := readFile(s.caPath())
	if err != nil {
		return nil, err
	}
	roots := x509.NewCertPool()
	if !roots.AppendCertsFromPEM(caPEM) {
		return nil, fmt.Errorf("%s: holds no certificate", s.caPath())
	}
	cfg := &tls.Config{
		MinVersion: tls.VersionTLS12,
		RootCAs:    roots,
		ServerName: s.ServerName,
		GetClientCertificate: func(*tls.CertificateRequestInfo) (*tls.Certificate, error) {
			cert, err := os.ReadFile(s.certPath())
			if err != nil {
				return nil, errors.New("the client certificate could not be read")
			}
			key, err := os.ReadFile(s.keyPath())
			if err != nil {
				return nil, errors.New("the client key could not be read")
			}
			pair, err := tls.X509KeyPair(cert, key)
			if err != nil {
				return nil, errors.New("the client key no longer matches the certificate")
			}
			return &pair, nil
		},
	}
	return credentials.NewTLS(cfg), nil
}

// Close shuts the connection.
func (c *Client) Close() error { return c.conn.Close() }

// callError keeps a gRPC status code with fixed local text, never the server's message. status.FromError still reads the code, so the writer (C) classifies the call;
// retry trailers reach the caller through a forwarded grpc.Trailer.
type callError struct {
	code codes.Code
	text string
}

func (e *callError) Error() string              { return e.text }
func (e *callError) GRPCStatus() *status.Status { return status.New(e.code, e.text) }

// sanitize drops the server's message, keeping the code and local text.
func sanitize(err error) error {
	if err == nil {
		return nil
	}
	st, _ := status.FromError(err)
	return &callError{st.Code(), "nico-api call failed with " + st.Code().String()}
}

// Version returns the nico-api build version. A failed call keeps its gRPC code; opts reach the RPC so a caller can collect trailers.
func (c *Client) Version(ctx context.Context, opts ...grpc.CallOption) (string, error) {
	ctx, cancel := context.WithTimeout(ctx, c.timeout)
	defer cancel()
	info, err := c.forge.Version(ctx, &corev1.VersionRequest{DisplayConfig: false}, opts...)
	if err != nil {
		return "", sanitize(err)
	}
	return info.GetBuildVersion(), nil
}

// The five expected-inventory writes. Each forwards the caller's CallOptions (so C can pass grpc.Trailer), bounds the RPC, and returns a sanitized error.

func (c *Client) AddExpectedRackGroup(ctx context.Context, body *corev1.ExpectedRackGroup, opts ...grpc.CallOption) error {
	ctx, cancel := context.WithTimeout(ctx, c.timeout)
	defer cancel()
	_, err := c.forge.AddExpectedRackGroup(ctx, body, opts...)
	return sanitize(err)
}

func (c *Client) AddExpectedRack(ctx context.Context, body *corev1.ExpectedRack, opts ...grpc.CallOption) error {
	ctx, cancel := context.WithTimeout(ctx, c.timeout)
	defer cancel()
	_, err := c.forge.AddExpectedRack(ctx, body, opts...)
	return sanitize(err)
}

func (c *Client) AddExpectedMachine(ctx context.Context, body *corev1.ExpectedMachine, opts ...grpc.CallOption) error {
	ctx, cancel := context.WithTimeout(ctx, c.timeout)
	defer cancel()
	_, err := c.forge.AddExpectedMachine(ctx, body, opts...)
	return sanitize(err)
}

func (c *Client) AddExpectedSwitch(ctx context.Context, body *corev1.ExpectedSwitch, opts ...grpc.CallOption) error {
	ctx, cancel := context.WithTimeout(ctx, c.timeout)
	defer cancel()
	_, err := c.forge.AddExpectedSwitch(ctx, body, opts...)
	return sanitize(err)
}

func (c *Client) AddExpectedPowerShelf(ctx context.Context, body *corev1.ExpectedPowerShelf, opts ...grpc.CallOption) error {
	ctx, cancel := context.WithTimeout(ctx, c.timeout)
	defer cancel()
	_, err := c.forge.AddExpectedPowerShelf(ctx, body, opts...)
	return sanitize(err)
}
