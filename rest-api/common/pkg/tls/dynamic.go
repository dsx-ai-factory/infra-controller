// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tls

import (
	"bytes"
	"crypto/tls"
	"crypto/x509"
	"encoding/pem"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"sync"
	"time"

	"github.com/sirupsen/logrus"
)

var (
	refreshPeriod = 30 * time.Second
)

// DynTLSCfg implements a periodically refreshed tls config that can be
// used by servers and clients
type DynTLSCfg struct {
	sync.Mutex
	keyPath    string
	certPath   string
	cacertPath string
	tlsCfg     *tls.Config

	cachedCert   *tls.Certificate
	cachedCa     []byte
	observedCa   []byte // Last client CA contents observed, not the installed trust pool.
	caCertPool   *x509.CertPool
	cachedCfg    *tls.Config
	cacheUpdated bool

	isClient bool
	ticker   *time.Ticker
	stop     chan bool
	stopOnce sync.Once
	logger   *logrus.Logger
}

// NewDynTLSCfg returns a DynTLSCfg
func NewDynTLSCfg(keyPath, certPath, cacertPath string) (*DynTLSCfg, error) {
	d := &DynTLSCfg{
		keyPath:    keyPath,
		certPath:   certPath,
		cacertPath: cacertPath,
		logger:     logrus.New(),
	}
	d.logger.SetFormatter(&logrus.TextFormatter{
		FullTimestamp:   true,
		TimestampFormat: "2006-01-02T15:04:05.999Z07:00",
		CallerPrettyfier: func(f *runtime.Frame) (string, string) {
			return "", fmt.Sprintf("%s:%d", filepath.Base(f.File), f.Line)
		},
	})
	d.logger.SetReportCaller(true)

	caCert, err := os.ReadFile(cacertPath)
	if err != nil {
		return nil, err
	}
	d.caCertPool, err = parseCABundle(caCert, d.logger.WithField("path", cacertPath))
	if err != nil {
		return nil, fmt.Errorf("failed to parse CA certificate %s: %w", cacertPath, err)
	}
	d.cachedCa = caCert
	d.observedCa = caCert
	cert, err := tls.LoadX509KeyPair(certPath, keyPath)
	if err != nil {
		return nil, err
	}
	d.cachedCert = &cert
	d.tlsCfg = &tls.Config{MinVersion: tls.VersionTLS12}

	d.ticker = time.NewTicker(refreshPeriod)
	d.stop = make(chan bool)
	go d.pollCerts()
	return d, nil
}

// parseCABundle preserves AppendCertsFromPEM's tolerance of malformed blocks.
// Strict validation is diagnostic only; a bundle needs at least one usable certificate.
func parseCABundle(data []byte, logger logrus.FieldLogger) (*x509.CertPool, error) {
	pool := x509.NewCertPool()
	if !pool.AppendCertsFromPEM(data) {
		return nil, fmt.Errorf("CA bundle contains no usable certificates")
	}
	err := validateCABundle(data)
	if err != nil {
		logger.Errorf("CA bundle contains malformed certificate blocks; using parseable certificates: %v", err)
	}
	return pool, nil
}

// validateCABundle detects malformed certificate blocks, including those pem.Decode skips.
func validateCABundle(data []byte) error {
	marker := []byte("-----BEGIN CERTIFICATE-----")
	// Count only PEM boundary lines, not marker text in explanatory comments.
	remaining := 0
	for _, line := range bytes.Split(data, []byte("\n")) {
		if bytes.Equal(bytes.TrimRight(line, "\r \t"), marker) {
			remaining++
		}
	}
	for len(data) > 0 {
		block, rest := pem.Decode(data)
		if block == nil {
			break
		}
		data = rest
		if block.Type != "CERTIFICATE" {
			continue
		}
		remaining--
		if len(block.Headers) != 0 {
			return fmt.Errorf("malformed certificate PEM block")
		}
		_, err := x509.ParseCertificate(block.Bytes)
		if err != nil {
			return fmt.Errorf("invalid certificate: %w", err)
		}
	}
	if remaining != 0 {
		return fmt.Errorf("malformed certificate PEM block")
	}
	return nil
}

// Close stops the poller go routine
func (d *DynTLSCfg) Close() {
	d.stopOnce.Do(func() {
		d.ticker.Stop()
		close(d.stop)
	})
}

// WithTLSCfg allows a tls config to be passed in
func (d *DynTLSCfg) WithTLSCfg(cfg *tls.Config) *DynTLSCfg {
	d.Lock()
	defer d.Unlock()
	d.tlsCfg = cfg
	return d
}

// ClientCfg returns tls config that can be used for a tls client
// CA cannot be refreshed for clients. Instead a warning is logged.
func (d *DynTLSCfg) ClientCfg() *tls.Config {
	d.Lock()
	defer d.Unlock()
	d.isClient = true

	d.tlsCfg.RootCAs = d.caCertPool
	d.tlsCfg.Certificates = []tls.Certificate(nil)
	d.tlsCfg.GetClientCertificate = func(_ *tls.CertificateRequestInfo) (*tls.Certificate, error) {
		d.Lock()
		defer d.Unlock()
		return d.cachedCert, nil
	}
	return d.tlsCfg
}

// ServerCfg returns a tls config that can be used by tls servers. The
// config including CA is synced with the source files.
func (d *DynTLSCfg) ServerCfg() *tls.Config {
	d.tlsCfg.Certificates = nil

	// getter for the server config for any given client
	d.tlsCfg.GetConfigForClient = func(_ *tls.ClientHelloInfo) (*tls.Config, error) {
		d.Lock()
		defer d.Unlock()
		if d.cachedCfg == nil || d.cacheUpdated {
			d.cachedCfg = d.tlsCfg.Clone()
			d.cachedCfg.Certificates = []tls.Certificate{*d.cachedCert}
			d.cachedCfg.RootCAs = d.caCertPool
			d.cachedCfg.ClientCAs = d.caCertPool
			d.cacheUpdated = false
		}

		return d.cachedCfg, nil
	}

	return d.tlsCfg
}

func (d *DynTLSCfg) pollCerts() {
	for {
		select {
		case <-d.stop:
			return
		case <-d.ticker.C:
			d.refresh()
		}
	}
}

func (d *DynTLSCfg) refresh() {
	d.Lock()
	defer d.Unlock()

	// Keep the last valid trust pool on CA refresh failures. They must not
	// prevent identity renewal or poison otherwise usable handshake configs.
	caCert, err := os.ReadFile(d.cacertPath)
	if err != nil {
		d.logger.Errorf("Failed to read CA certificate from %s - %v", d.cacertPath, err)
	} else if d.isClient {
		if !bytes.Equal(caCert, d.observedCa) {
			if bytes.Equal(caCert, d.cachedCa) {
				d.logger.Info("CA file matches the installed client trust pool again")
			} else {
				d.logger.Warn("CA has changed, clients will likely not work without restart")
			}
			d.observedCa = caCert
		}
	} else if !bytes.Equal(caCert, d.cachedCa) {
		caCertPool, err := parseCABundle(caCert, d.logger.WithField("path", d.cacertPath))
		if err != nil {
			d.logger.Errorf("Failed to parse CA certificate %s - %v", d.cacertPath, err)
		} else {
			d.caCertPool = caCertPool
			d.cachedCa = caCert
			d.cacheUpdated = true
			d.logger.Info("Updated server CA certificate")
		}
	}

	// Keep the last usable identity if files are temporarily missing or mismatched.
	cert, err := tls.LoadX509KeyPair(d.certPath, d.keyPath)
	if err != nil {
		d.logger.Errorf("Failed to read certificate and key from %s, %s - %v", d.certPath, d.keyPath, err)
		return
	}

	if !reflect.DeepEqual(&cert, d.cachedCert) {
		d.logger.Info("Updated certificate")
		d.cachedCert = &cert
		d.cacheUpdated = true
	}
}
