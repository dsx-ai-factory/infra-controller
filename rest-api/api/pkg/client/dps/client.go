// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package dps provides the narrow client contract used by the REST API to
// associate allocated machines with DPS resource groups and power policies.
package dps

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"os"
	"strings"
	"time"

	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/keepalive"
)

// ErrResourceGroupAlreadyExists reports that DPS rejected creation because the
// requested resource-group name is already in use.
var ErrResourceGroupAlreadyExists = errors.New("DPS resource group already exists")

const (
	authorizationHeader = "authorization"
	keepaliveTime       = 30 * time.Second
	keepaliveTimeout    = 10 * time.Second
)

// Config describes a remote DPS connection. TLS and bearer-token
// authentication are mandatory when DPS integration is enabled.
type Config struct {
	Endpoint       string
	RequestTimeout time.Duration
	TokenPath      string
	CAPath         string
	ServerName     string
}

// PolicyProvider is the DPS policy discovery behavior used to validate NICo
// power-profile names. Implementations return the names from ListPolicies.
type PolicyProvider interface {
	ListPowerProfiles(ctx context.Context) ([]string, error)
}

// ResourceGroupProvisioner is the DPS behavior required by VPC and instance
// lifecycle handlers. machineID is both the NICo/Core Machine ID and the DPS
// topology resource name.
type ResourceGroupProvisioner interface {
	ValidateAllocation(ctx context.Context, machineIDs []string, powerProfile string) error
	CreateResourceGroup(ctx context.Context, resourceGroup string, externalID int64) error
	DeleteResourceGroup(ctx context.Context, resourceGroup string) error
	AddMachine(ctx context.Context, resourceGroup, machineID, powerProfile string) error
	AddMachines(ctx context.Context, resourceGroup string, machineIDs []string, powerProfile string) error
	UpdateMachineProfile(ctx context.Context, resourceGroup, machineID, powerProfile string) error
	RemoveMachine(ctx context.Context, resourceGroup, machineID string) error
	RemoveMachines(ctx context.Context, resourceGroup string, machineIDs []string) error
	ActivateResourceGroup(ctx context.Context, resourceGroup string) error
}

// PowerProvisioner is the complete narrow DPS contract used by the REST API.
type PowerProvisioner interface {
	PolicyProvider
	ResourceGroupProvisioner
}

type tokenCredentials struct {
	path string
}

// NewTokenCredentials returns per-RPC credentials that reload the bearer token
// from disk for each request, allowing secret rotation without restarting the
// API service.
func NewTokenCredentials(path string) credentials.PerRPCCredentials {
	return tokenCredentials{path: path}
}

func (c tokenCredentials) GetRequestMetadata(context.Context, ...string) (map[string]string, error) {
	token, err := os.ReadFile(c.path)
	if err != nil {
		return nil, fmt.Errorf("read DPS token: %w", err)
	}

	trimmedToken := strings.TrimSpace(string(token))
	if trimmedToken == "" {
		return nil, fmt.Errorf("DPS token file is empty")
	}

	return map[string]string{authorizationHeader: "Bearer " + trimmedToken}, nil
}

func (tokenCredentials) RequireTransportSecurity() bool {
	return true
}

// NewConnection creates a reusable authenticated gRPC connection to DPS.
// Establishment remains lazy; individual operations must apply Config's
// RequestTimeout to their contexts.
func NewConnection(config Config) (*grpc.ClientConn, error) {
	caPEM, err := os.ReadFile(config.CAPath)
	if err != nil {
		return nil, fmt.Errorf("read DPS CA certificate: %w", err)
	}

	rootCAs := x509.NewCertPool()
	if !rootCAs.AppendCertsFromPEM(caPEM) {
		return nil, fmt.Errorf("DPS CA certificate file contains no valid certificates")
	}

	tlsConfig := &tls.Config{
		MinVersion: tls.VersionTLS12,
		RootCAs:    rootCAs,
		ServerName: config.ServerName,
	}

	connection, err := grpc.NewClient(
		config.Endpoint,
		grpc.WithTransportCredentials(credentials.NewTLS(tlsConfig)),
		grpc.WithPerRPCCredentials(NewTokenCredentials(config.TokenPath)),
		grpc.WithUnaryInterceptor(unaryTracePropagator()),
		grpc.WithStreamInterceptor(streamTracePropagator()),
		grpc.WithKeepaliveParams(keepalive.ClientParameters{
			Time:                keepaliveTime,
			Timeout:             keepaliveTimeout,
			PermitWithoutStream: true,
		}),
	)
	if err != nil {
		return nil, fmt.Errorf("create DPS gRPC client: %w", err)
	}

	return connection, nil
}
