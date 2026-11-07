// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package pki

import (
	"context"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/cert-manager/pkg/types"
)

// NativeCertificateIssuer implements types.CertificateIssuer using native Go crypto
type NativeCertificateIssuer struct {
	ca      *CA
	baseDNS string
}

// NativeCertificateIssuerOptions defines options for the native issuer
type NativeCertificateIssuerOptions struct {
	BaseDNS        string
	CertificateTTL string
	CACommonName   string
	CAOrganization string
	CACertFile     string
	CAKeyFile      string
	AltCACertFile  string
	AltCAKeyFile   string
}

// NewNativeCertificateIssuer creates a new native Go certificate issuer.
func NewNativeCertificateIssuer(opts NativeCertificateIssuerOptions) (types.CertificateIssuer, error) {
	var ca *CA
	var err error
	var loadErr error

	// Try primary paths (vault-style)
	if opts.CACertFile != "" && opts.CAKeyFile != "" {
		ca, err = LoadCA(opts.CACertFile, opts.CAKeyFile)
		if err == nil {
			fmt.Printf("Loaded CA from primary path: %s\n", opts.CACertFile)
			return &NativeCertificateIssuer{
				ca:      ca,
				baseDNS: opts.BaseDNS,
			}, nil
		}
		loadErr = fmt.Errorf("primary path (%s): %w", opts.CACertFile, err)
	}

	// Try alternate paths
	if opts.AltCACertFile != "" && opts.AltCAKeyFile != "" {
		ca, err = LoadCA(opts.AltCACertFile, opts.AltCAKeyFile)
		if err == nil {
			fmt.Printf("Loaded CA from alternate path: %s\n", opts.AltCACertFile)
			return &NativeCertificateIssuer{
				ca:      ca,
				baseDNS: opts.BaseDNS,
			}, nil
		}
		if loadErr != nil {
			loadErr = fmt.Errorf("%w; alternate path (%s): %w", loadErr, opts.AltCACertFile, err)
		} else {
			loadErr = fmt.Errorf("alternate path (%s): %w", opts.AltCACertFile, err)
		}
	}

	// No CA found - error
	if loadErr != nil {
		return nil, fmt.Errorf("CA certificate required but not found: %w", loadErr)
	}
	return nil, fmt.Errorf("CA certificate required: no paths configured")
}

// NewCertificate implements types.CertificateIssuer
func (i *NativeCertificateIssuer) NewCertificate(ctx context.Context, req *types.CertificateRequest) (string, string, error) {
	sans := req.UniqueName(i.baseDNS)
	ttl := req.TTL
	if ttl == 0 {
		ttl = 24 * 90 // 90 days default
	}
	return i.ca.IssueCertificate(sans, nil, ttl)
}

// RawCertificate implements types.CertificateIssuer
func (i *NativeCertificateIssuer) RawCertificate(ctx context.Context, sans []string, ttl int) (string, string, error) {
	// Guards the indexing below. IssueCertificate rejects a list that is empty
	// only once blank entries are discarded.
	if len(sans) == 0 {
		return "", "", fmt.Errorf("at least one SAN is required to issue a certificate")
	}
	return i.ca.IssueCertificate(sans[0], sans[1:], ttl)
}

// GetCACertificate implements types.CertificateIssuer
func (i *NativeCertificateIssuer) GetCACertificate(ctx context.Context) (string, error) {
	return i.ca.GetCACertificatePEM(), nil
}

// GetCRL implements types.CertificateIssuer
func (i *NativeCertificateIssuer) GetCRL(ctx context.Context) (string, error) {
	return i.ca.GetCRL(), nil
}
