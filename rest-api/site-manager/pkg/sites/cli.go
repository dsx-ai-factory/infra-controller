// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sites

import (
	"time"

	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	cli "github.com/urfave/cli/v2"
)

const (
	defOTPDuration = 24
)

// NewCommand creates a new cli command
func NewCommand() *cli.Command {
	return &cli.Command{
		Name:  "Forge Site Manager Service",
		Usage: "Forge Site Manager Service",
		Flags: []cli.Flag{
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
				Name:  "creds-manager-ca-path",
				Value: "/etc/credsmgr-ca/ca.crt",
				Usage: "CA that signed the creds manager endpoint's listener. That " +
					"endpoint's host name must appear in its --dns-name SANs",
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
				Value:   "nico-rest",
				EnvVars: []string{"SITE_MANAGER_NS"},
				Usage:   "Namespace where service is deployed",
			},
			&cli.IntFlag{
				Name:  "otp-duration",
				Value: defOTPDuration,
				Usage: "OTP duration in hours",
			},
			// Accepted so existing invocations keep working. Site Manager
			// doesn't report to Sentry.
			&cli.StringFlag{
				Name:    "sentry-dsn",
				Value:   "",
				EnvVars: []string{"SENTRY_DSN"},
				Usage:   "Deprecated and ignored",
			},
		},
		// TODO: cmd/site-manager builds its cli.App without this Before, so --debug
		// has no effect. Wiring it would turn on debug logging in every
		// default deployment, because the Helm chart and the kustomize base
		// pass --debug.
		Before: func(c *cli.Context) error {
			if c.Bool("debug") {
				zerolog.SetGlobalLevel(zerolog.DebugLevel)
			}
			return nil
		},
		Action: func(c *cli.Context) error {
			ctx := c.Context

			sentryDSN := c.String("sentry-dsn")
			if sentryDSN != "" {
				log.Warn().Msg("--sentry-dsn / SENTRY_DSN is deprecated and ignored")
			}

			otpHrs := c.Int("otp-duration")
			if otpHrs != defOTPDuration {
				log.Warn().Int("otp_duration_hours", otpHrs).Msg("OTP duration overridden")
			}

			o := Options{
				credsMgrURL:    c.String("creds-manager-url"),
				credsMgrCAPath: c.String("creds-manager-ca-path"),
				ingressHost:    c.String("ingress-host"),
				listenPort:     c.String("listen-port"),
				tlsKeyPath:     c.String("tls-key-path"),
				tlsCertPath:    c.String("tls-cert-path"),
				namespace:      c.String("namespace"),
				otpDuration:    time.Duration(otpHrs) * time.Hour,
			}
			log.Info().Msg("configuring Site Manager server")
			s, err := NewSiteManager(ctx, o)
			if err != nil {
				return err
			}
			log.Info().Msg("starting Site Manager server")
			s.Start(ctx)

			<-ctx.Done()

			gracePeriod := 5 * time.Second
			log.Info().Dur("grace_period", gracePeriod).Msg("shutdown requested, waiting for the grace period")
			time.Sleep(gracePeriod)
			log.Info().Msg("server terminated")
			return nil
		},
	}
}
