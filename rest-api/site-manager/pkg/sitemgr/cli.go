// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sitemgr

import (
	"time"

	"github.com/sirupsen/logrus"
	cli "github.com/urfave/cli/v2"

	"github.com/NVIDIA/infra-controller/rest-api/cert-manager/pkg/core"
)

const (
	vaultSecretsRootPathDefault = "/vault/secrets"
	defOTPDuration              = 24
)

var (
	otpDuration = defOTPDuration * time.Hour
)

// NewCommand creates a new cli command
func NewCommand() *cli.Command {
	return &cli.Command{
		Name:  "Forge Site Manager Service",
		Usage: "Forge Site Manager Service",
		Flags: []cli.Flag{
			&cli.StringFlag{
				Name:  "creds-manager-token-file",
				Value: "/var/run/secrets/nico-rest-cert-manager/token",
				Usage: "Projected service account token file for audience nico-rest-cert-manager; reloaded by the client",
			},
			&cli.StringFlag{
				Name:  "creds-manager-ca-file",
				Value: "/etc/pki/creds-manager/ca.crt",
				Usage: "PEM CA bundle used to verify the certificate manager TLS certificate",
			},
			&cli.StringFlag{
				Name:  "creds-manager-server-name",
				Value: "credsmgr.csm",
				Usage: "TLS server name expected from certificate manager; empty uses the URL host",
			},
			&cli.BoolFlag{
				Name:  "debug",
				Usage: "Log debug message to stderr",
				Value: false,
			},
			&cli.StringFlag{
				Name:  "listen-port",
				Value: "8100",
				Usage: "TLS port to listen to",
			},
			&cli.StringFlag{
				Name:  "ingress-host",
				Value: "sitemgr.forge.nvidia.com",
				Usage: "Ingress host name for self signed certs",
			},
			&cli.StringFlag{
				Name:  "creds-manager-url",
				Value: "https://localhost:8000",
				Usage: "creds manager service endpoint used by backend",
			},
			&cli.StringFlag{
				Name:  "tls-key-path",
				Value: "",
				Usage: "File path for server tls key",
			},
			&cli.StringFlag{
				Name:  "tls-cert-path",
				Value: "",
				Usage: "File path for server tls cert",
			},
			&cli.StringFlag{
				Name:    "namespace",
				Value:   "carbide-rest",
				EnvVars: []string{"SITE_MANAGER_NS"},
				Usage:   "Namespace where service is deployed",
			},
			&cli.IntFlag{
				Name:  "otp-duration",
				Value: defOTPDuration,
				Usage: "OTP duration in hours",
			},
			&cli.StringFlag{
				Name:    "sentry-dsn",
				Value:   "",
				EnvVars: []string{"SENTRY_DSN"},
				Usage:   "DSN for sentry/glitchtip",
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

			o := Options{
				credsMgrTokenFile:  c.String("creds-manager-token-file"),
				credsMgrCAFile:     c.String("creds-manager-ca-file"),
				credsMgrServerName: c.String("creds-manager-server-name"),
				credsMgrURL:        c.String("creds-manager-url"),
				ingressHost:        c.String("ingress-host"),
				listenPort:         c.String("listen-port"),
				tlsKeyPath:         c.String("tls-key-path"),
				tlsCertPath:        c.String("tls-cert-path"),
				namespace:          c.String("namespace"),
				sentryDSN:          c.String("sentry-dsn"),
			}

			otpHrs := c.Int("otp-duration")
			if otpHrs != defOTPDuration {
				log.Warnf("OTP duration overridden to %v hours", otpHrs)
			}
			otpDuration = time.Duration(otpHrs) * time.Hour
			log.Info("Configuring site manager server")
			s, err := NewSiteManager(ctx, o)
			if err != nil {
				return err
			}
			log.Info("Starting site manager server")
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
