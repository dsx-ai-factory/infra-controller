// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package certs

import (
	"time"

	"github.com/sirupsen/logrus"
	cli "github.com/urfave/cli/v2"
	authenticationclient "k8s.io/client-go/kubernetes/typed/authentication/v1"
	"k8s.io/client-go/rest"

	"github.com/NVIDIA/infra-controller/rest-api/cert-manager/pkg/core"
	"github.com/NVIDIA/infra-controller/rest-api/cert-manager/pkg/pki"
)

const (
	caBaseDNSDefault = "temporal.local"
)

// NewCommand creates a new cli command
func NewCommand() *cli.Command {
	return &cli.Command{
		Name:  "NICo Credentials Service",
		Usage: "NICo Credentials Service",
		Flags: []cli.Flag{
			&cli.StringFlag{
				Name:     "allowed-service-account",
				Usage:    "Only this Kubernetes identity may issue certificates (system:serviceaccount:namespace:name)",
				Required: true,
			},
			&cli.BoolFlag{
				Name:  "debug",
				Usage: "Log debug message to stderr",
				Value: false,
			},
			&cli.StringFlag{
				Name:  "tls-port",
				Value: "8000",
				Usage: "TLS port to listen to",
			},
			&cli.StringFlag{
				Name:  "insecure-port",
				Value: "8001",
				Usage: "http port to listen to",
			},
			&cli.StringFlag{
				Name:  "dns-name",
				Value: "credsmgr.csm",
				Usage: "DNS name for incluster tls access",
			},
			&cli.StringFlag{
				Name:  "ca-base-dns",
				Value: caBaseDNSDefault,
				Usage: "Base dns appended to common names",
			},
			&cli.StringFlag{
				Name:    "sentry-dsn",
				Value:   "",
				EnvVars: []string{"SENTRY_DSN"},
				Usage:   "DSN for sentry/glitchtip",
			},
			&cli.StringFlag{
				Name:    "ca-common-name",
				Value:   "NICo Local CA",
				EnvVars: []string{"CA_COMMON_NAME"},
				Usage:   "Common name for the CA certificate",
			},
			&cli.StringFlag{
				Name:    "ca-organization",
				Value:   "NVIDIA",
				EnvVars: []string{"CA_ORGANIZATION"},
				Usage:   "Organization for the CA certificate",
			},
			&cli.StringFlag{
				Name:    "ca-cert-file",
				Value:   "/vault/secrets/vault-root-ca-certificate/certificate",
				EnvVars: []string{"CA_CERT_FILE"},
				Usage:   "Path to CA certificate file",
			},
			&cli.StringFlag{
				Name:    "ca-key-file",
				Value:   "/vault/secrets/vault-root-ca-private-key/privatekey",
				EnvVars: []string{"CA_KEY_FILE"},
				Usage:   "Path to CA private key file",
			},
			&cli.StringFlag{
				Name:    "alt-ca-cert-file",
				Value:   "/etc/pki/ca/tls.crt",
				EnvVars: []string{"ALT_CA_CERT_FILE"},
				Usage:   "Alternate path to CA certificate file",
			},
			&cli.StringFlag{
				Name:    "alt-ca-key-file",
				Value:   "/etc/pki/ca/tls.key",
				EnvVars: []string{"ALT_CA_KEY_FILE"},
				Usage:   "Alternate path to CA private key file",
			},
		},
		Before: func(c *cli.Context) error {
			if c.Bool("debug") {
				core.GetLogger(c.Context).Logger.SetLevel(logrus.DebugLevel)
			}
			return nil
		},
		Action: func(c *cli.Context) error {
			ctx := c.Context
			log := core.GetLogger(ctx)

			cfg, err := rest.InClusterConfig()
			if err != nil {
				return err
			}
			cfg.Timeout = 5 * time.Second
			authClient, err := authenticationclient.NewForConfig(cfg)
			if err != nil {
				return err
			}

			o := Options{
				AllowedServiceAccount: c.String("allowed-service-account"),
				TokenReviewer:         authClient.TokenReviews(),
				Addr:                  ":" + c.String("tls-port"),
				InsecureAddr:          ":" + c.String("insecure-port"),
				DNSName:               c.String("dns-name"),
				CABaseDNS:             c.String("ca-base-dns"),
				sentryDSN:             c.String("sentry-dsn"),
			}

			// Use native Go PKI for certificate generation
			log.Info("Using native Go PKI for certificate generation")
			caCertFile := c.String("ca-cert-file")
			caKeyFile := c.String("ca-key-file")
			altCACertFile := c.String("alt-ca-cert-file")
			altCAKeyFile := c.String("alt-ca-key-file")
			log.Infof("CA paths - primary: %s, alternate: %s", caCertFile, altCACertFile)

			issuer, err := pki.NewNativeCertificateIssuer(pki.NativeCertificateIssuerOptions{
				BaseDNS:        c.String("ca-base-dns"),
				CACommonName:   c.String("ca-common-name"),
				CAOrganization: c.String("ca-organization"),
				CACertFile:     caCertFile,
				CAKeyFile:      caKeyFile,
				AltCACertFile:  altCACertFile,
				AltCAKeyFile:   altCAKeyFile,
			})
			if err != nil {
				log.Errorf("Failed to create native PKI issuer: %v", err)
				return err
			}
			log.Info("Native PKI issuer initialized successfully")

			log.Info("Configuring Certificate server")
			s, err := NewServerWithIssuer(ctx, o, issuer)
			if err != nil {
				return err
			}
			log.Info("Starting Certificate server")
			s.Start(ctx)

			<-ctx.Done()

			gracePeriod := 5 * time.Second
			log.Infof("Shut down requested, wait for %v grace period ...", gracePeriod)
			time.Sleep(gracePeriod)
			log.Infof("Server terminated.")
			return nil
		},
	}
}
