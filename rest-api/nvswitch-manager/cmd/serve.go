// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"syscall"
	"time"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"

	log "github.com/sirupsen/logrus"

	"github.com/spf13/cobra"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
	svc "github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/internal/service"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/credentials"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/nvswitchmanager"
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
	defaultDataStoreType = nvswitchmanager.DatastoreTypeInMemory

	// default db config
	defaultDbPort     = 5432
	defaultDbHostName = "localhost"
	defaultDbName     = "nsmdatabase"

	defaultDbUser     = "nsmuser"
	defaultDbPassword = "nsmpassword"

	// default vault config
	defaultVaultToken   = "nsmvaultroot"
	defaultVaultAddress = "http://127.0.0.1:8201"

	// default firmware config
	defaultFirmwarePackagesDir = ""
	defaultFirmwareFirmwareDir = ""
	defaultFirmwareNumWorkers  = 10
	defaultFirmwarePollSeconds = 5
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
	firmwarePackagesDir string
	firmwareFirmwareDir string
	firmwareNumWorkers  int
	firmwarePollSeconds int
)

// serveCmd represents the serve command
var serveCmd = &cobra.Command{
	Use:   "serve",
	Short: "Start the gRPC server",
	Long:  `Start the gRPC server to allow other services to manage NV-Switch trays`,
	Run: func(cmd *cobra.Command, args []string) {
		doServe()
	},
}

func init() {
	rootCmd.AddCommand(serveCmd)

	// Flags with environment variable fallbacks for Kubernetes deployment compatibility.
	// Environment variables take precedence over defaults, CLI flags take precedence over env vars.
	serveCmd.Flags().IntVarP(&port, "port", "p", getEnvIntOrDefault("NSM_PORT", defaultServicePort), "Port for the gRPC server (env: NSM_PORT)") //nolint
	serveCmd.Flags().StringVarP(&datastoreType, "datastore", "d", string(defaultDataStoreType), "DataStore Type")

	serveCmd.Flags().StringVarP(&dbUser, "db_user", "u", getEnvOrDefault("DB_USER", defaultDbUser), "DB User (env: DB_USER)")
	serveCmd.Flags().StringVarP(&dbPassword, "db_password", "b", getEnvOrDefault("DB_PASSWORD", defaultDbPassword), "DB Password (env: DB_PASSWORD)")
	serveCmd.Flags().IntVarP(&dbPort, "db_port", "r", getEnvIntOrDefault("DB_PORT", defaultDbPort), "DB Port (env: DB_PORT)") //nolint
	serveCmd.Flags().StringVarP(&dbHostName, "db_host", "o", getEnvOrDefault("DB_ADDR", defaultDbHostName), "DB Host Name (env: DB_ADDR)")
	serveCmd.Flags().StringVarP(&dbName, "db_name", "n", getEnvOrDefault("DB_DATABASE", defaultDbName), "DB Name (env: DB_DATABASE)")
	serveCmd.Flags().StringVarP(&dbCertPath, "db_cert_path", "c", getEnvOrDefault("DB_CERT_PATH", ""), "DB CA Certificate Path (env: DB_CERT_PATH)")

	serveCmd.Flags().StringVarP(&vaultToken, "vault_token", "t", getEnvOrDefault("VAULT_TOKEN", defaultVaultToken), "Vault Token (env: VAULT_TOKEN)")
	serveCmd.Flags().StringVarP(&vaultAddress, "vault_address", "a", getEnvOrDefault("VAULT_ADDR", defaultVaultAddress), "Vault Address (env: VAULT_ADDR)")

	// Firmware manager flags
	serveCmd.Flags().StringVar(&firmwarePackagesDir, "fw_bundles_dir", getEnvOrDefault("FW_BUNDLES_DIR", defaultFirmwarePackagesDir), "Firmware bundles directory (env: FW_BUNDLES_DIR)")
	serveCmd.Flags().StringVar(&firmwareFirmwareDir, "fw_firmware_dir", getEnvOrDefault("FW_FIRMWARE_DIR", defaultFirmwareFirmwareDir), "Firmware files directory (env: FW_FIRMWARE_DIR)")
	serveCmd.Flags().IntVar(&firmwareNumWorkers, "fw_workers", getEnvIntOrDefault("FW_WORKERS", defaultFirmwareNumWorkers), "Number of firmware update workers (env: FW_WORKERS)")
	serveCmd.Flags().IntVar(&firmwarePollSeconds, "fw_poll_seconds", getEnvIntOrDefault("FW_POLL_SECONDS", defaultFirmwarePollSeconds), "Worker poll interval in seconds (env: FW_POLL_SECONDS)")
}

func doServe() {
	ctx := context.Background()

	// Initialize tracing so the otelgrpc server handler emits real spans;
	// enabled purely by OTEL_* env vars
	otelShutdown, otelErr := cotel.Bootstrap(ctx, cotel.ExporterConfigured(), "nico-nvswitch-manager")
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
			DataStoreType: nvswitchmanager.DataStoreType(strings.ToLower(datastoreType)),
			VaultConf: credentials.VaultConfig{
				Address: vaultAddress,
				Token:   vaultToken,
			},
			DBConf: db.Config{
				Host:              dbHostName,
				Port:              dbPort,
				DBName:            dbName,
				Credential:        credential.New(dbUser, dbPassword),
				CACertificatePath: dbCertPath,
			},
			FirmwareConf: svc.FirmwareConfig{
				PackagesDir:       firmwarePackagesDir,
				FirmwareDir:       firmwareFirmwareDir,
				NumWorkers:        firmwareNumWorkers,
				SchedulerInterval: time.Duration(firmwarePollSeconds) * time.Second,
			},
		},
	)

	if firmwarePackagesDir != "" {
		log.Printf("Firmware config: packages_dir=%s, firmware_dir=%s, workers=%d, poll_seconds=%d",
			firmwarePackagesDir, firmwareFirmwareDir, firmwareNumWorkers, firmwarePollSeconds)
	}

	if err != nil {
		log.Fatalf("failed to create the new gRPC server: %v\n", err)
	}

	log.Printf("New service is created with port: %+v, data store type: %s, vault address: %s", port, datastoreType, vaultAddress)

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
