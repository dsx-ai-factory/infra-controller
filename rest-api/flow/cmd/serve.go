// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"fmt"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/spf13/cobra"
	"go.temporal.io/sdk/worker"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/authz"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/secret"
	svc "github.com/NVIDIA/infra-controller/rest-api/flow/internal/service"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager"
	cmbuiltin "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/builtin"
	cmconfig "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/config"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/readiness"
	temporalmanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/manager"
	pkgcerts "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/certs"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

const (
	defaultServicePort                 = 50051
	componentMgrCfgEnvVar              = "COMPONENT_MANAGER_CONFIG"
	allowedServiceIdentitiesFileEnvVar = "FLOW_AUTHORIZATION_ALLOWED_SERVICE_IDENTITIES_FILE"
	authorizationModeEnvVar            = "FLOW_AUTHORIZATION_MODE"

	// computeImplEnvVar selects the compute component manager
	// implementation at deploy time. This override exists for the
	// migration from the legacy machine-centric NICo RPCs ("nicolegacy")
	// to Core's Component Manager dispatch ("nico"). The value must be a
	// compute implementation name registered in the catalog (currently
	// "nico", "nicolegacy", or "mock"). When the variable is unset or
	// empty the embedded service config selection is used.
	//
	// The name mirrors COMPONENT_MANAGER_CONFIG above: a future per-type
	// override for nvswitch / powershelf would simply add
	// COMPONENT_MANAGER_NVSWITCH / COMPONENT_MANAGER_POWERSHELF.
	//
	// TODO: remove this override and the compute/nicolegacy package once
	// every Flow deployment runs on the Component Manager path.
	computeImplEnvVar = "COMPONENT_MANAGER_COMPUTE"
)

var (
	port                     int
	componentMgrConfig       string
	devMode                  bool
	allowedServiceIdentities []string

	// clientOnlyFlags are the global persistent flags that apply only to
	// client commands. They are hidden from serve's help and rejected if set.
	clientOnlyFlags = []string{flagHost, flagPort}

	// serveCmd represents the serve command
	serveCmd = &cobra.Command{
		Use:   "serve",
		Short: "Start the Flow gRPC server",
		Long:  `Start the gRPC server to allow other services to manage the racks`,
		PreRunE: func(cmd *cobra.Command, args []string) error {
			for _, name := range clientOnlyFlags {
				if cmd.Root().PersistentFlags().Changed(name) {
					return fmt.Errorf("--%s is not applicable to 'flow serve'", name)
				}
			}
			return nil
		},
		Run: func(cmd *cobra.Command, args []string) {
			doServe()
		},
	}
)

func init() {
	rootCmd.AddCommand(serveCmd)

	// Hide client-only persistent flags from serve's help output.
	for _, name := range clientOnlyFlags {
		_ = serveCmd.InheritedFlags().MarkHidden(name)
	}

	serveCmd.Flags().IntVarP(&port, "listen-port", "p", defaultServicePort, "Port for the gRPC server") //nolint:lll
	// Component manager config: priority is CLI flag > env var > service default config.
	serveCmd.Flags().StringVarP(&componentMgrConfig, "component-config", "c", "", "Path to component manager config file (YAML)")               //nolint:lll
	serveCmd.Flags().BoolVar(&devMode, "dev-mode", false, "Enable developer options (gRPC reflection, debug logging). Not for production use.") //nolint:lll
	serveCmd.Flags().StringSliceVar(
		&allowedServiceIdentities,
		"allowed-service-identity",
		nil,
		"SPIFFE URI allowed to call Flow; may be specified multiple times",
	)
}

// loadComponentManagerConfig loads the component manager configuration with the following priority:
//
//  1. CLI flag: --component-config / -c <path>
//     Example: ./flow serve -c /etc/flow/custom.yaml
//
//  2. Environment variable: COMPONENT_MANAGER_CONFIG=<path>
//     Example: COMPONENT_MANAGER_CONFIG=/etc/flow/componentmanager.yaml
//
//  3. Embedded default: builtin service config
//     Used when no config file is provided. The primary production path.
//     Uses the component manager implementation map defined by builtin.
//
// After the base config is selected, COMPONENT_MANAGER_COMPUTE (if
// set) overrides the compute component manager selection. This narrow
// override exists so deployments can flip between the legacy and the
// new Component Manager-based compute implementations without shipping
// a separate config file. See computeImplEnvVar.
//
// The config specifies:
//   - Which component manager implementations to use (nico, nicolegacy, mock)
//   - Provider settings (timeouts, endpoints)
func loadComponentManagerConfig() (cmconfig.Config, error) {
	// Priority 1: CLI flag
	configPath := componentMgrConfig

	// Priority 2: Environment variable
	if configPath == "" {
		configPath = os.Getenv(componentMgrCfgEnvVar)
	}

	var (
		cfg cmconfig.Config
		err error
	)
	if configPath != "" {
		log.Info().Str("config_path", configPath).Msg("Loading component manager config from file")
		cfg, err = cmbuiltin.LoadConfig(configPath)
	} else {
		log.Info().Msg("Using embedded component manager service config")
		cfg, err = cmbuiltin.LoadConfig("")
	}
	if err != nil {
		return cmconfig.Config{}, err
	}

	applyComputeImplementationOverride(&cfg)

	return cfg, nil
}

// applyComputeImplementationOverride mutates cfg in place to honour the
// COMPONENT_MANAGER_COMPUTE env var when it is set to a non-empty
// value. The catalog still validates the resulting selection during
// registry construction, so an invalid implementation name surfaces as
// a normal startup failure rather than being silently ignored.
func applyComputeImplementationOverride(cfg *cmconfig.Config) {
	override := strings.TrimSpace(os.Getenv(computeImplEnvVar))
	if override == "" {
		return
	}

	if cfg.ComponentManagers == nil {
		cfg.ComponentManagers = map[devicetypes.ComponentType]string{}
	}

	previous := cfg.ComponentManagers[devicetypes.ComponentTypeCompute]
	cfg.ComponentManagers[devicetypes.ComponentTypeCompute] = override

	log.Info().
		Str("env_var", computeImplEnvVar).
		Str("previous_implementation", previous).
		Str("implementation", override).
		Msg("Compute component manager implementation overridden by environment")
}

// doServe is the main entry point for the serve subcommand. It loads all
// configuration, initialises provider and component manager registries, builds
// the service, and blocks until a termination signal is received.
func doServe() {
	if devMode {
		zerolog.SetGlobalLevel(zerolog.DebugLevel)
	} else {
		zerolog.SetGlobalLevel(zerolog.InfoLevel)
	}

	if os.Getenv(svc.EnvVarName) == "" {
		log.Warn().Msgf("%s not set, defaulting to %q for local development", svc.EnvVarName, "development")
		os.Setenv(svc.EnvVarName, "development") //nolint:errcheck
	}

	flowEnv, err := svc.GetDeploymentEnv()
	if err != nil {
		log.Fatal().Err(err).Msg("Invalid deployment environment")
	}

	log.Info().Str(svc.EnvVarName, flowEnv).Msg("Deployment environment")

	flowConfig := config.ReadConfig()

	authorization, err := loadAuthorizationConfig()
	if err != nil {
		log.Fatal().Err(err).Msg("failed to load gRPC service authorization config")
	}

	dbConf, err := cdb.ConfigFromEnv()
	if err != nil {
		log.Fatal().Msgf("failed to retrieve DB conn information: %v", err)
	}

	temporalConf, err := svc.BuildTemporalConfigFromEnv()
	if err != nil {
		log.Fatal().Msgf("failed to retrieve Temporal conn information: %v", err)
	}

	dataCipher, err := loadDataCipherFromEnv()
	if err != nil {
		log.Fatal().Err(err).Msg("failed to load Flow data encryption key")
	}
	if dataCipher == nil {
		log.Warn().
			Str("env_var", secret.EncryptionKeyPathEnvVar).
			Msg(
				"Flow data encryption key is not configured; requests with firmware " +
					"authentication data and existing encrypted operations will fail",
			)
	}

	ctx := context.Background()

	// Load component manager configuration
	cmConfig, err := loadComponentManagerConfig()
	if err != nil {
		log.Fatal().Msgf("failed to load component manager config: %v", err)
	}

	// Initialize provider registry (creates API clients based on config)
	providerRegistry, err := cmbuiltin.NewProviderRegistry(ctx, cmConfig)
	if err != nil {
		log.Fatal().Msgf("failed to initialize provider registry: %v", err)
	}

	// Open a DB session for the readiness gate. The gate consults the
	// persisted ComponentOperationStatus inventorysync writes to the component
	// table, so it must share the same database the service will migrate
	// on startup. The deferred Close runs after doServe returns, i.e.
	// after the service has fully stopped.
	//
	// This is a separate pool from the one svc.New opens. Consolidating
	// onto a single shared session is tracked as follow-up cleanup; the
	// gate's query footprint is negligible so a second pool is acceptable
	// in the interim.
	readinessSession, err := cdb.NewSessionFromConfig(ctx, dbConf)
	if err != nil {
		log.Fatal().Msgf("failed to open DB session for readiness gate: %v", err)
	}
	defer readinessSession.Close()

	readinessGate := readiness.NewDBGate(
		readiness.NewDBReader(readinessSession.DB),
		readiness.DefaultWaitTimeout,
		readiness.DefaultPollInterval,
	)

	// Initialize component manager registry
	cmRegistry, err := cmbuiltin.NewComponentManagerRegistry(
		cmConfig,
		providerRegistry,
		readinessGate,
	)
	if err != nil {
		log.Fatal().Msgf("failed to initialize component manager registry: %v", err)
	}
	logComponentManagerRegistry(cmRegistry)

	temporalManagerConf := temporalmanager.Config{
		ClientConf: *temporalConf,
		WorkerOptions: map[string]worker.Options{
			temporalmanager.WorkflowQueue: {},
		},
		ComponentManagerRegistry: cmRegistry,
		DataCipher:               dataCipher,
	}

	if os.Getenv("REPORT_NICO_API_VERSION") != "" {
		// Do some basic nico-api requests, mainly for early testing; this code can be removed when we're doing actual communication
		go func() {
			client, err := nicoapi.NewClient(time.Minute)
			if err != nil {
				log.Fatal().Msgf("Unable to create GRPC client: %v", err)
			}
			for {
				time.Sleep(time.Second * 10)
				if version, err := client.Version(ctx); err != nil {
					log.Error().Msgf("Unable to retrieve version from nico-core-api: %v", err)
					continue
				} else {
					log.Info().Msgf("nico-core-api version: %s", version)
					break
				}
			}
			for {
				time.Sleep(time.Second * 10)
				if machines, err := client.GetMachines(ctx); err != nil {
					log.Error().Msgf("Unable to retrieve machines from nico-core-api: %v", err)
					continue
				} else {
					log.Info().Msgf("nico-core-api machines: %v", machines)
					break
				}
			}
		}()
	}

	service, err := svc.New(
		ctx,
		svc.Config{
			Port:             port,
			DBConf:           dbConf,
			ExecutorConf:     &temporalManagerConf,
			FlowConfig:       flowConfig,
			CMConfig:         cmConfig,
			ProviderRegistry: providerRegistry,
			DataCipher:       dataCipher,
			DevMode:          devMode,
			Authorization:    authorization,
			CertConfig: pkgcerts.Config{
				CACert:  globalCACert,
				TLSCert: globalTLSCert,
				TLSKey:  globalTLSKey,
			},
		},
	)

	if err != nil {
		log.Fatal().Msgf("failed to create the new gRPC server: %v", err)
	}

	log.Info().Msg("New Flow service is created\n")
	log.Info().Msgf("DB config: %+v", dbConf)
	log.Info().Msgf("Temporal config: %+v", temporalManagerConf.ClientConf)

	sigs := make(chan os.Signal, 1)
	signal.Notify(sigs, syscall.SIGINT, syscall.SIGTERM)
	go func() {
		<-sigs // Block execution until signal from terminal gets triggered here.
		service.Stop(ctx)
	}()

	if err := service.Start(ctx); err != nil {
		log.Fatal().Msgf("failed to start the service: %v\n", err)
	}
}

func loadDataCipherFromEnv() (*secret.Cipher, error) {
	path, configured := os.LookupEnv(secret.EncryptionKeyPathEnvVar)
	if !configured {
		return nil, nil
	}

	path = strings.TrimSpace(path)
	if path == "" {
		return nil, fmt.Errorf("%s is set but empty", secret.EncryptionKeyPathEnvVar)
	}

	return secret.NewCipherFromFile(path)
}

func loadAuthorizationConfig() (authz.Config, error) {
	mode, err := authz.NewMode(os.Getenv(authorizationModeEnvVar))
	if err != nil {
		return authz.Config{}, fmt.Errorf("%s: %w", authorizationModeEnvVar, err)
	}

	rawPath, isSet := os.LookupEnv(allowedServiceIdentitiesFileEnvVar)
	if !isSet {
		// Without an allowlist file, use identities passed through the
		// --allowed-service-identity command-line option.
		return authz.Config{
			AllowedServiceIdentities: allowedServiceIdentities,
			Mode:                     mode,
		}, nil
	}

	if len(allowedServiceIdentities) > 0 {
		return authz.Config{}, fmt.Errorf(
			"allowed service identities cannot be configured by both file and command-line options",
		)
	}

	path := strings.TrimSpace(rawPath)
	data, err := os.ReadFile(path)
	if err != nil {
		return authz.Config{}, fmt.Errorf("read allowed service identities file %q: %w", path, err)
	}

	var identities []string
	for line := range strings.SplitSeq(string(data), "\n") {
		identity := strings.TrimSpace(line)
		if identity != "" {
			identities = append(identities, identity)
		}
	}

	return authz.Config{
		AllowedServiceIdentities: identities,
		Mode:                     mode,
	}, nil
}

func logComponentManagerRegistry(registry *componentmanager.Registry) {
	descriptors, err := registry.Descriptors()
	if err != nil {
		log.Warn().
			Err(err).
			Msg("Component manager registry report unavailable")
		return
	}

	for _, descriptor := range descriptors {
		log.Info().
			Str(
				"component_type",
				devicetypes.ComponentTypeToString(descriptor.Type),
			).
			Str("implementation", descriptor.Implementation).
			Strs("capabilities", descriptor.Capabilities.Strings()).
			Msg("Active component manager capabilities")
	}
}
