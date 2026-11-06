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
				Name:  "cert-manager-url",
				Value: "https://localhost:8000",
				Usage: "REST Cert Manager endpoint that issues Site certificates",
			},
			&cli.StringFlag{
				Name:  "cert-manager-ca-path",
				Value: "/etc/credsmgr-ca/ca.crt",
				Usage: "CA that signed REST Cert Manager's listener. The endpoint's " +
					"host name must appear in its --dns-name SANs",
			},
			// Former names of --cert-manager-url and --cert-manager-ca-path,
			// accepted so existing invocations keep working.
			&cli.StringFlag{
				Name:  "creds-manager-url",
				Usage: "Deprecated, use --cert-manager-url",
			},
			&cli.StringFlag{
				Name:  "creds-manager-ca-path",
				Usage: "Deprecated, use --cert-manager-ca-path",
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

			o := newOptions(c)
			log.Info().Msg("configuring Site Manager server")
			crdClient, err := newCRDClient()
			if err != nil {
				return err
			}
			s, err := NewSiteManager(ctx, o, crdClient)
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

// newOptions builds the Site Manager options from the parsed flags.
func newOptions(c *cli.Context) Options {
	return Options{
		certManagerURL:    renamedString(c, "cert-manager-url", "creds-manager-url"),
		certManagerCAPath: renamedString(c, "cert-manager-ca-path", "creds-manager-ca-path"),
		ingressHost:       c.String("ingress-host"),
		listenPort:        c.String("listen-port"),
		tlsKeyPath:        c.String("tls-key-path"),
		tlsCertPath:       c.String("tls-cert-path"),
		namespace:         c.String("namespace"),
		otpDuration:       time.Duration(c.Int("otp-duration")) * time.Hour,
	}
}

// renamedString returns the value of the flag name. A value given through its
// former name oldName is used only when name itself is not set.
func renamedString(c *cli.Context, name, oldName string) string {
	if !c.IsSet(oldName) {
		return c.String(name)
	}
	if c.IsSet(name) {
		log.Warn().Msgf("--%s is deprecated and ignored because --%s is set", oldName, name)
		return c.String(name)
	}
	log.Warn().Msgf("--%s is deprecated, use --%s", oldName, name)
	return c.String(oldName)
}
