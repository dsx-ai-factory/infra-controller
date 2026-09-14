// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"crypto/ed25519"
	"crypto/rand"
	"crypto/x509"
	"encoding/pem"
	"math/big"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	dto "github.com/prometheus/client_model/go"
	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/managerapi"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/conftypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes"
)

func TestWorkflowOrchestrator(t *testing.T) {
	caPublicKey, caKey, err := ed25519.GenerateKey(rand.Reader)
	require.NoError(t, err)
	ca := &x509.Certificate{
		SerialNumber:          big.NewInt(1),
		NotBefore:             time.Date(2020, 1, 1, 0, 0, 0, 0, time.UTC),
		NotAfter:              time.Date(2035, 1, 1, 0, 0, 0, 0, time.UTC),
		IsCA:                  true,
		BasicConstraintsValid: true,
		KeyUsage:              x509.KeyUsageCertSign,
	}
	caDER, err := x509.CreateCertificate(rand.Reader, ca, ca, caPublicKey, caKey)
	require.NoError(t, err)
	publicKey, key, err := ed25519.GenerateKey(rand.Reader)
	require.NoError(t, err)
	keyDER, err := x509.MarshalPKCS8PrivateKey(key)
	require.NoError(t, err)
	keyPEM := pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: keyDER})
	expiration := time.Date(2030, 1, 1, 0, 0, 0, 0, time.UTC)

	tests := []struct {
		name         string
		master       bool
		reload       bool
		failedReload bool
		omitLeaf     bool
		invalidKey   bool
		nilGauge     bool
	}{
		{name: "master loads existing client certificate", master: true},
		{name: "non-master loads existing client certificate"},
		{name: "reload updates client expiration", reload: true},
		{name: "load and reload without cached leaf", omitLeaf: true, reload: true},
		{name: "failed reload retains previous expiration", failedReload: true},
		{name: "invalid key leaves metric unchanged", invalidKey: true},
		{name: "missing gauge does not interrupt certificate loading", nilGauge: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if tt.omitLeaf {
				t.Setenv("GODEBUG", os.Getenv("GODEBUG")+",x509keypairleaf=0")
			}
			previousAccess := ManagerAccess
			previousGauge := CertExpirationMetric
			previousRegisterer := prometheus.DefaultRegisterer
			t.Cleanup(func() {
				ManagerAccess = previousAccess
				CertExpirationMetric = previousGauge
				prometheus.DefaultRegisterer = previousRegisterer
			})

			registry := prometheus.NewRegistry()
			prometheus.DefaultRegisterer = registry
			conf := &conftypes.Config{
				EnableTLS:        true,
				DisableBootstrap: true,
				IsMasterPod:      tt.master,
				MetricsNamespace: conftypes.DefaultMetricsNamespace,
				Temporal: conftypes.TemporalConfig{
					TemporalCertPath: t.TempDir(),
				},
			}
			data := &elektratypes.Elektra{Log: zerolog.Nop(), Managers: managertypes.NewManagerType()}
			managerConf := &managerapi.ManagerConf{EB: conf}
			NewWorkflowManager(data, nil, managerConf).Init()
			gauge := CertExpirationMetric
			gaugeValue := func() float64 {
				t.Helper()
				metric := &dto.Metric{}
				writeErr := gauge.Write(metric)
				require.NoError(t, writeErr)
				return metric.GetGauge().GetValue()
			}
			require.Zero(t, gaugeValue())
			if tt.nilGauge {
				CertExpirationMetric = nil
			}

			writeCertificate := func(notAfter time.Time) {
				t.Helper()
				cert := &x509.Certificate{
					SerialNumber: big.NewInt(notAfter.Unix()),
					NotBefore:    ca.NotBefore,
					NotAfter:     notAfter,
					KeyUsage:     x509.KeyUsageDigitalSignature,
					ExtKeyUsage:  []x509.ExtKeyUsage{x509.ExtKeyUsageClientAuth},
				}
				certDER, certErr := x509.CreateCertificate(rand.Reader, cert, ca, publicKey, caKey)
				require.NoError(t, certErr)
				chain := pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: certDER})
				chain = append(chain, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: caDER})...)
				certErr = os.WriteFile(conf.Temporal.GetTemporalClientCertFullPath(), chain, 0600)
				require.NoError(t, certErr)
			}
			err = os.MkdirAll(filepath.Dir(conf.Temporal.GetTemporalClientCertFullPath()), 0700)
			require.NoError(t, err)
			writeCertificate(expiration)
			err = os.WriteFile(conf.Temporal.GetTemporalClientKeyFullPath(), keyPEM, 0600)
			require.NoError(t, err)
			if tt.invalidKey {
				err = os.WriteFile(conf.Temporal.GetTemporalClientKeyFullPath(), []byte("invalid PEM"), 0600)
				require.NoError(t, err)
			}

			// Omit the CA file to stop after loading the client certificate, before
			// dialing Temporal. The chain has different leaf and CA expirations.
			loadErr := workflowOrchestrator()
			if tt.invalidKey {
				require.ErrorContains(t, loadErr, "PEM data in key input")
				assert.Zero(t, gaugeValue())
				return
			}
			var pathErr *os.PathError
			require.ErrorAs(t, loadErr, &pathErr)
			assert.Equal(t, filepath.Join(conf.Temporal.TemporalCertPath, "ca", "ca.crt"), pathErr.Path)
			if tt.nilGauge {
				assert.Zero(t, gaugeValue())
				return
			}
			assert.Equal(t, float64(expiration.Unix()), gaugeValue())
			metrics, gatherErr := registry.Gather()
			require.NoError(t, gatherErr)
			require.Len(t, metrics, 4)
			var expirationMetric *dto.MetricFamily
			for _, metric := range metrics {
				if metric.GetName() == "nico_rest_site_agent_temporal_cert_expiration" {
					expirationMetric = metric
					break
				}
			}
			require.NotNil(t, expirationMetric)
			require.Len(t, expirationMetric.GetMetric(), 1)
			assert.Equal(t, float64(expiration.Unix()), expirationMetric.GetMetric()[0].GetGauge().GetValue())
			if tt.failedReload {
				err = os.WriteFile(conf.Temporal.GetTemporalClientKeyFullPath(), []byte("invalid PEM"), 0600)
				require.NoError(t, err)
				loadErr = workflowOrchestrator()
				require.ErrorContains(t, loadErr, "PEM data in key input")
				assert.Equal(t, float64(expiration.Unix()), gaugeValue())
			}
			if tt.reload {
				newExpiration := expiration.Add(24 * time.Hour)
				writeCertificate(newExpiration)
				loadErr = workflowOrchestrator()
				require.ErrorAs(t, loadErr, &pathErr)
				assert.Equal(t, float64(newExpiration.Unix()), gaugeValue())
			}
		})
	}
}
