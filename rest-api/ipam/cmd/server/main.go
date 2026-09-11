/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package main

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"fmt"
	"log"
	"log/slog"
	"os"

	"time"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	goipam "github.com/NVIDIA/infra-controller/rest-api/ipam"
	"github.com/metal-stack/v"
	"github.com/urfave/cli/v2"
	"go.mongodb.org/mongo-driver/mongo/options"
)

func main() {
	// Initialize tracing so the otelconnect interceptor and storage clients
	// emit real spans; enabled purely by OTEL_* env vars
	otelShutdown, otelErr := cotel.Bootstrap(context.Background(), cotel.ExporterConfigured(), "nico-ipam")
	if otelErr != nil {
		log.Printf("failed to initialize tracing: %v", otelErr)
	} else {
		defer func() {
			shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			defer cancel()
			if err := otelShutdown(shutdownCtx); err != nil {
				log.Printf("failed to shut down tracing: %v", err)
			}
		}()
	}

	app := &cli.App{
		Name:    "go-ipam server",
		Usage:   "grpc server for go ipam",
		Version: v.V.String(),
		Flags: []cli.Flag{
			&cli.StringFlag{
				Name:    "grpc-server-endpoint",
				Value:   "localhost:9090",
				Usage:   "gRPC server endpoint",
				EnvVars: []string{"GOIPAM_GRPC_SERVER_ENDPOINT"},
			},
			&cli.StringFlag{
				Name:    "metrics-endpoint",
				Value:   "localhost:2112",
				Usage:   "metrics endpoint",
				EnvVars: []string{"GOIPAM_METRICS_ENDPOINT"},
			},
			&cli.StringFlag{
				Name:    "log-level",
				Value:   "info",
				Usage:   "log-level can be one of error|warn|info|debug",
				EnvVars: []string{"GOIPAM_LOG_LEVEL"},
			},
			&cli.StringFlag{
				Name:    "server-tls-cert",
				Usage:   "path to TLS certificate for the gRPC/HTTP server",
				EnvVars: []string{"GOIPAM_SERVER_TLS_CERT"},
			},
			&cli.StringFlag{
				Name:    "server-tls-key",
				Usage:   "path to TLS private key for the gRPC/HTTP server",
				EnvVars: []string{"GOIPAM_SERVER_TLS_KEY"},
			},
		},
		Commands: []*cli.Command{
			{
				Name:    "memory",
				Aliases: []string{"m"},
				Usage:   "start with memory backend",
				Action: func(ctx *cli.Context) error {
					c := getConfig(ctx)
					c.Storage = goipam.NewMemory(ctx.Context)
					s := newServer(c)
					return s.Run()
				},
			},
			{
				Name:    "file",
				Aliases: []string{"f", "local"},
				Usage:   "start with local JSON file backend",
				Flags: []cli.Flag{
					&cli.StringFlag{
						Name:        "path",
						Value:       goipam.DefaultLocalFilePath,
						DefaultText: "~/.local/share/go-ipam/ipam-db.json",
						Usage:       "path to the file",
						EnvVars:     []string{"GOIPAM_FILE_PATH"},
					},
				},
				Action: func(ctx *cli.Context) error {
					c := getConfig(ctx)
					c.Storage = goipam.NewLocalFile(ctx.Context, ctx.String("path"))
					s := newServer(c)
					return s.Run()
				},
			},
			{
				Name:    "postgres",
				Aliases: []string{"pg"},
				Usage:   "start with postgres backend",
				Flags: []cli.Flag{
					&cli.StringFlag{
						Name:    "host",
						Value:   "localhost",
						Usage:   "postgres db hostname",
						EnvVars: []string{"GOIPAM_PG_HOST"},
					},
					&cli.StringFlag{
						Name:    "port",
						Value:   "5432",
						Usage:   "postgres db port",
						EnvVars: []string{"GOIPAM_PG_PORT"},
					},
					&cli.StringFlag{
						Name:    "user",
						Value:   "go-ipam",
						Usage:   "postgres db user",
						EnvVars: []string{"GOIPAM_PG_USER"},
					},
					&cli.StringFlag{
						Name:    "password",
						Value:   "secret",
						Usage:   "postgres db password",
						EnvVars: []string{"GOIPAM_PG_PASSWORD"},
					},
					&cli.StringFlag{
						Name:    "dbname",
						Value:   "goipam",
						Usage:   "postgres db name",
						EnvVars: []string{"GOIPAM_PG_DBNAME"},
					},
					&cli.StringFlag{
						Name:    "sslmode",
						Value:   "disable",
						Usage:   "postgres sslmode, possible values: disable|require|verify-ca|verify-full",
						EnvVars: []string{"GOIPAM_PG_SSLMODE"},
					},
				},
				Action: func(ctx *cli.Context) error {
					c := getConfig(ctx)
					host := ctx.String("host")
					port := ctx.String("port")
					user := ctx.String("user")
					password := ctx.String("password")
					dbname := ctx.String("dbname")
					sslmode := ctx.String("sslmode")
					pgStorage, err := goipam.NewPostgresStorage(host, port, user, password, dbname, goipam.SSLMode(sslmode))
					if err != nil {
						return err
					}
					c.Storage = pgStorage
					s := newServer(c)
					return s.Run()
				},
			},
			{
				Name:  "redis",
				Usage: "start with redis backend",
				Flags: []cli.Flag{
					&cli.StringFlag{
						Name:    "host",
						Value:   "localhost",
						Usage:   "redis db hostname",
						EnvVars: []string{"GOIPAM_REDIS_HOST"},
					},
					&cli.StringFlag{
						Name:    "port",
						Value:   "6379",
						Usage:   "redis db port",
						EnvVars: []string{"GOIPAM_REDIS_PORT"},
					},
					&cli.StringFlag{
						Name:    "username",
						Usage:   "redis username (ACL)",
						EnvVars: []string{"GOIPAM_REDIS_USERNAME"},
					},
					&cli.StringFlag{
						Name:    "password",
						Usage:   "redis password",
						EnvVars: []string{"GOIPAM_REDIS_PASSWORD"},
					},
					&cli.StringFlag{
						Name:    "tls-cert",
						Usage:   "path to TLS client certificate for redis",
						EnvVars: []string{"GOIPAM_REDIS_TLS_CERT"},
					},
					&cli.StringFlag{
						Name:    "tls-key",
						Usage:   "path to TLS client key for redis",
						EnvVars: []string{"GOIPAM_REDIS_TLS_KEY"},
					},
					&cli.StringFlag{
						Name:    "tls-ca",
						Usage:   "path to CA certificate for redis TLS verification",
						EnvVars: []string{"GOIPAM_REDIS_TLS_CA"},
					},
				},
				Action: func(ctx *cli.Context) error {
					c := getConfig(ctx)
					cfg := goipam.RedisConfig{
						IP:       ctx.String("host"),
						Port:     ctx.String("port"),
						Username: ctx.String("username"),
						Password: ctx.String("password"),
					}

					tlsCert := ctx.String("tls-cert")
					tlsKey := ctx.String("tls-key")
					caPath := ctx.String("tls-ca")
					if tlsCert != "" || tlsKey != "" || caPath != "" {
						if (tlsCert == "") != (tlsKey == "") {
							return fmt.Errorf("both redis tls-cert and tls-key must be set together")
						}

						tlsCfg := &tls.Config{}
						if caPath != "" {
							caCert, err := os.ReadFile(caPath)
							if err != nil {
								return fmt.Errorf("failed to read redis CA cert: %w", err)
							}
							pool := x509.NewCertPool()
							if !pool.AppendCertsFromPEM(caCert) {
								return fmt.Errorf("failed to parse redis CA cert")
							}
							tlsCfg.RootCAs = pool
						}
						if tlsCert != "" {
							cert, err := tls.LoadX509KeyPair(tlsCert, tlsKey)
							if err != nil {
								return fmt.Errorf("failed to load redis TLS cert/key: %w", err)
							}
							tlsCfg.Certificates = []tls.Certificate{cert}
						}
						cfg.TLSConfig = tlsCfg
					}

					var err error
					c.Storage, err = goipam.NewRedisFromConfig(ctx.Context, cfg)
					if err != nil {
						return err
					}

					s := newServer(c)
					return s.Run()
				},
			},
			{
				Name:  "etcd",
				Usage: "start with etcd backend",
				Flags: []cli.Flag{
					&cli.StringFlag{
						Name:    "host",
						Value:   "localhost",
						Usage:   "etcd db hostname",
						EnvVars: []string{"GOIPAM_ETCD_HOST"},
					},
					&cli.StringFlag{
						Name:    "port",
						Value:   "2379",
						Usage:   "etcd db port",
						EnvVars: []string{"GOIPAM_ETCD_PORT"},
					},
					&cli.StringFlag{
						Name:    "cert-file",
						Value:   "cert.pem",
						Usage:   "etcd cert file",
						EnvVars: []string{"GOIPAM_ETCD_CERT_FILE"},
					},
					&cli.StringFlag{
						Name:    "key-file",
						Value:   "key.pem",
						Usage:   "etcd key file",
						EnvVars: []string{"GOIPAM_ETCD_KEY_FILE"},
					},
					&cli.BoolFlag{
						Name:    "insecure-skip-verify",
						Value:   false,
						Usage:   "skip tls certification verification",
						EnvVars: []string{"GOIPAM_ETCD_INSECURE_SKIP_VERIFY"},
					},
				},
				Action: func(ctx *cli.Context) error {
					c := getConfig(ctx)
					host := ctx.String("host")
					port := ctx.String("port")
					certFile := ctx.String("cert-file")
					keyFile := ctx.String("key-file")
					cert, err := os.ReadFile(certFile)
					if err != nil {
						return err
					}
					key, err := os.ReadFile(keyFile)
					if err != nil {
						return err
					}
					insecureSkip := ctx.Bool("insecure-skip-verify")

					c.Storage, err = goipam.NewEtcd(ctx.Context, host, port, cert, key, insecureSkip)
					if err != nil {
						return err
					}
					s := newServer(c)
					return s.Run()
				},
			},
			{
				Name:  "mongodb",
				Usage: "start with mongodb backend",
				Flags: []cli.Flag{
					&cli.StringFlag{
						Name:    "host",
						Value:   "localhost",
						Usage:   "mongodb db hostname",
						EnvVars: []string{"GOIPAM_MONGODB_HOST"},
					},
					&cli.StringFlag{
						Name:    "port",
						Value:   "27017",
						Usage:   "mongodb db port",
						EnvVars: []string{"GOIPAM_MONGODB_PORT"},
					},
					&cli.StringFlag{
						Name:    "db-name",
						Value:   "go-ipam",
						Usage:   "mongodb db name",
						EnvVars: []string{"GOIPAM_MONGODB_DB_NAME"},
					},
					&cli.StringFlag{
						Name:    "collection-name",
						Value:   "prefixes",
						Usage:   "mongodb db collection name",
						EnvVars: []string{"GOIPAM_MONGODB_COLLECTION_NAME"},
					},
					&cli.StringFlag{
						Name:    "user",
						Value:   "mongodb",
						Usage:   "mongodb db user",
						EnvVars: []string{"GOIPAM_MONGODB_USER"},
					},
					&cli.StringFlag{
						Name:    "password",
						Value:   "mongodb",
						Usage:   "mongodb db password",
						EnvVars: []string{"GOIPAM_MONGODB_PASSWORD"},
					},
				},
				Action: func(ctx *cli.Context) error {
					c := getConfig(ctx)
					host := ctx.String("host")
					port := ctx.String("port")
					user := ctx.String("user")
					password := ctx.String("password")
					dbname := ctx.String("db-name")

					opts := options.Client()
					opts.ApplyURI(fmt.Sprintf(`mongodb://%s:%s`, host, port))
					opts.Auth = &options.Credential{
						AuthMechanism: `SCRAM-SHA-1`,
						Username:      user,
						Password:      password,
					}

					mongocfg := goipam.MongoConfig{
						DatabaseName:       dbname,
						MongoClientOptions: opts,
					}
					db, err := goipam.NewMongo(context.Background(), mongocfg)
					if err != nil {
						return err
					}
					c.Storage = db

					s := newServer(c)
					return s.Run()
				},
			},
		},
	}

	err := app.Run(os.Args)
	if err != nil {
		log.Fatalf("Error in cli: %v", err)
	}

}

func getConfig(ctx *cli.Context) config {
	opts := &slog.HandlerOptions{
		Level: slog.LevelInfo,
	}
	switch ctx.String("log-level") {
	case "debug":
		opts.Level = slog.LevelDebug
	case "error":
		opts.Level = slog.LevelError
	}
	return config{
		GrpcServerEndpoint: ctx.String("grpc-server-endpoint"),
		MetricsEndpoint:    ctx.String("metrics-endpoint"),
		Log:                slog.New(slog.NewJSONHandler(os.Stdout, opts)),
		TLSCertFile:        ctx.String("server-tls-cert"),
		TLSKeyFile:         ctx.String("server-tls-key"),
	}
}
