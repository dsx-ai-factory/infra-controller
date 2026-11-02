// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package temporal

import (
	"crypto/tls"
	"path/filepath"

	dynamictls "github.com/NVIDIA/infra-controller/rest-api/common/pkg/tls"
	pkgcerts "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/certs"
)

const (
	caCertificateFileName     = "ca.crt"
	clientCertificateFileName = "tls.crt"
	clientKeyFileName         = "tls.key"
)

func buildTLSConfig(c Config) (*tls.Config, *dynamictls.DynTLSCfg, error) {
	if !c.EnableTLS {
		return nil, nil, nil
	}

	return pkgcerts.Config{
		CACert:  filepath.Join(c.Endpoint.CACertificatePath, caCertificateFileName),
		TLSCert: filepath.Join(c.Endpoint.CACertificatePath, clientCertificateFileName),
		TLSKey:  filepath.Join(c.Endpoint.CACertificatePath, clientKeyFileName),
	}.DynamicTLSConfig(c.ServerName)
}
