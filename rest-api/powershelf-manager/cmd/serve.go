// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"os"
	"os/signal"
	"strconv"
	"syscall"
	"time"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"

	log "github.com/sirupsen/logrus"

	"github.com/spf13/cobra"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	svc "github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/internal/service"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/credentials"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/powershelfmanager"
)

// getEnvOrDefault returns the value of an environment variable or a default value.
func getEnvOrDefault(envVar, defaultVal string) string {
	if val := os.Getenv(envVar); val != "" {
		return val
	}
	return defaultVal
}

// getEnvIntOrDefault returns the int value of an environment variable or a default value.
func getEnvIntOrDefault(envVar string, defaultVal int) int {
	if val := os.Getenv(envVar); val != "" {
		if intVal, err := strconv.Atoi(val); err == nil {
			return intVal
		}
	}
	return defaultVal
}

const (
	// default service config
	defaultServicePort   = 50051
	defaultDataStoreType = powershelfmanager.DatastoreTypeInMemory

	// default db config
	defaultDbPort     = 5432
	defaultDbHostName = "localhost"
	defaultDbName     = "psmdatabase"

	defaultDbUser     = "psmuser"
	defaultDbPassword = "psmpassword"

	// default vault config
	defaultVaultToken   = "psmvaultroot"
	defaultVaultAddress = "http://127.0.0.1:8201"
)

var (
	// Service Config
	port          int
	datastoreType string

	// DB config
	dbUser     string
	dbPassword string
	dbPort     int
	dbHostName string
	dbName     string
	dbCertPath string

	// Vault config
	vaultToken   string
	vaultAddress string

	// Firmware config
	firmwareDir string
)

// serveCmd represents the serve command
var serveCmd = &cobra.Command{
	Use:   "serve",
	Short: "Start the gPRC server",
	Long:  `Start the gRPC server to allow other services to manage powershelves`,
	Run: func(cmd *cobra.Command, args []string) {
		doServe()
	},
}

func init() {
	rootCmd.AddCommand(serveCmd)

	// Flags with environment variable fallbacks for Kubernetes deployment compatibility.
	// Environment variables take precedence over defaults, CLI flags take precedence over env vars.
	// Env vars: DB_HOST, DB_PORT, DB_NAME, DB_USER, DB_PASSWORD, DB_CERT_PATH, VAULT_ADDR, VAULT_TOKEN
	serveCmd.Flags().IntVarP(&port, "port", "p", getEnvIntOrDefault("PSM_PORT", defaultServicePort), "Port for the gRPC server (env: PSM_PORT)") //nolint
	serveCmd.Flags().StringVarP(&datastoreType, "datastore", "d", string(defaultDataStoreType), "DataStore Type")

	serveCmd.Flags().StringVarP(&dbUser, "db_user", "u", getEnvOrDefault("DB_USER", defaultDbUser), "DB User (env: DB_USER)")
	serveCmd.Flags().StringVarP(&dbPassword, "db_password", "b", getEnvOrDefault("DB_PASSWORD", defaultDbPassword), "DB Password (env: DB_PASSWORD)")
	serveCmd.Flags().IntVarP(&dbPort, "db_port", "r", getEnvIntOrDefault("DB_PORT", defaultDbPort), "DB Port (env: DB_PORT)") //nolint
	serveCmd.Flags().StringVarP(&dbHostName, "db_host", "o", getEnvOrDefault("DB_HOST", defaultDbHostName), "DB Host Name (env: DB_HOST)")
	serveCmd.Flags().StringVarP(&dbName, "db_name", "n", getEnvOrDefault("DB_NAME", defaultDbName), "DB Name (env: DB_NAME)")
	serveCmd.Flags().StringVarP(&dbCertPath, "db_cert_path", "c", getEnvOrDefault("DB_CERT_PATH", ""), "DB CA Certificate Path (env: DB_CERT_PATH)")

	serveCmd.Flags().StringVarP(&vaultToken, "vault_token", "t", getEnvOrDefault("VAULT_TOKEN", defaultVaultToken), "Vault Token (env: VAULT_TOKEN)")
	serveCmd.Flags().StringVarP(&vaultAddress, "vault_address", "a", getEnvOrDefault("VAULT_ADDR", defaultVaultAddress), "Vault Address (env: VAULT_ADDR)")

	serveCmd.Flags().StringVar(&firmwareDir, "fw_dir", getEnvOrDefault("FW_DIR", "/var/lib/psm/firmware"), "Firmware files directory (env: FW_DIR)")
}

func doServe() {
	ctx := context.Background()

	// Initialize tracing so the otelgrpc server handler emits real spans;
	// enabled purely by OTEL_* env vars
	otelShutdown, otelErr := cotel.Bootstrap(ctx, cotel.ExporterConfigured(), "nico-powershelf-manager")
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
	service, err := svc.New(
		ctx,
		svc.Config{
			Port:          port,
			DataStoreType: powershelfmanager.DataStoreType(datastoreType),
			VaultConf: credentials.VaultConfig{
				Address: vaultAddress,
				Token:   vaultToken,
			},
			DBConf: cdb.Config{
				Host:              dbHostName,
				Port:              dbPort,
				DBName:            dbName,
				Credential:        credential.New(dbUser, dbPassword),
				CACertificatePath: dbCertPath,
			},
			FirmwareDir: firmwareDir,
		},
	)

	log.Printf("New service is created with port: %+v, data store type: %s, vault address: %s, firmware dir: %s", port, datastoreType, vaultAddress, firmwareDir)

	if err != nil {
		log.Fatalf("failed to create the new gRPC server: %v\n", err)
	}

	sigs := make(chan os.Signal, 1)
	signal.Notify(sigs, syscall.SIGINT, syscall.SIGTERM)
	go func() {
		<-sigs // Block execution until signal from terminal gets triggered here.
		service.Stop(ctx)
	}()

	if err := service.Start(ctx); err != nil {
		log.Fatalf("failed to start the service: %v\n", err)
	}
}
