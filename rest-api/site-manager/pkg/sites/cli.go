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
	defOTPDuration       = 24
	defCertManagerURL    = "https://localhost:8000"
	defCertManagerCAPath = "/etc/credsmgr-ca/ca.crt"
)

// NewCommand creates a new cli command
func NewCommand() *cli.Command {
	// Each REST Cert Manager flag shares its destination with its former name,
	// so whichever is given last wins, the same as repeating one flag. That
	// keeps an override appended under the former name working.
	var certManagerURL, certManagerCAPath string
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
				Name:        "cert-manager-url",
				Value:       defCertManagerURL,
				Destination: &certManagerURL,
				Usage:       "REST Cert Manager endpoint that issues Site certificates",
			},
			&cli.StringFlag{
				Name:        "cert-manager-ca-path",
				Value:       defCertManagerCAPath,
				Destination: &certManagerCAPath,
				Usage: "CA that signed REST Cert Manager's listener. The endpoint's " +
					"host name must appear in its --dns-name SANs",
			},
			// Former names of --cert-manager-url and --cert-manager-ca-path,
			// accepted so existing invocations keep working.
			&cli.StringFlag{
				Name:        "creds-manager-url",
				Value:       defCertManagerURL,
				Destination: &certManagerURL,
				Usage:       "Deprecated, use --cert-manager-url",
			},
			&cli.StringFlag{
				Name:        "creds-manager-ca-path",
				Value:       defCertManagerCAPath,
				Destination: &certManagerCAPath,
				Usage:       "Deprecated, use --cert-manager-ca-path",
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
	warnFormerName(c, "cert-manager-url", "creds-manager-url")
	warnFormerName(c, "cert-manager-ca-path", "creds-manager-ca-path")
	return Options{
		certManagerURL:    c.String("cert-manager-url"),
		certManagerCAPath: c.String("cert-manager-ca-path"),
		ingressHost:       c.String("ingress-host"),
		listenPort:        c.String("listen-port"),
		tlsKeyPath:        c.String("tls-key-path"),
		tlsCertPath:       c.String("tls-cert-path"),
		namespace:         c.String("namespace"),
		otpDuration:       time.Duration(c.Int("otp-duration")) * time.Hour,
	}
}

// warnFormerName logs a deprecation warning when the flag name was given under
// its former name oldName.
func warnFormerName(c *cli.Context, name, oldName string) {
	if c.IsSet(oldName) {
		log.Warn().Msgf("--%s is deprecated, use --%s", oldName, name)
	}
}
