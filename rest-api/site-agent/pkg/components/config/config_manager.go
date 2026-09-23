// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"flag"
	"fmt"
	"os"
	"regexp"
	"strconv"
	"strings"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/conftypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	swu "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/util"
	"github.com/google/uuid"
	"github.com/joho/godotenv"
	"github.com/rs/zerolog/log"
)

const (
	// Core gRPC default address and certificate paths
	DefaultCoreGrpcAddress        = "core-grpc.nico-system.svc.cluster.local:1079"
	DefaultCoreGrpcCACertPath     = "/etc/core-grpc/ca.crt"
	DefaultCoreGrpcClientCertPath = "/etc/core-grpc/tls.crt"
	DefaultCoreGrpcClientKeyPath  = "/etc/core-grpc/tls.key"

	// Flow uses the same SPIFFE trust domain (core-grpc.local) and vault-core-grpc-issuer as Core gRPC,
	// so we can reuse the Core gRPC certificates for mTLS with Flow gRPC.
	DefaultFlowGrpcAddress        = "flow.flow.svc.cluster.local:50051"
	DefaultFlowGrpcCACertPath     = "/etc/core-grpc/ca.crt"
	DefaultFlowGrpcClientCertPath = "/etc/core-grpc/tls.crt"
	DefaultFlowGrpcClientKeyPath  = "/etc/core-grpc/tls.key"
)

// NewElektraConfig reads configurations from env variables and returns
func NewElektraConfig(utMode bool) *conftypes.Config {
	log.Info().Msg("Config Manager: Processing Config")
	conf := conftypes.NewConfType()

	var enableDebug string
	var devmode string
	var enableTLS string
	var disableBootstrap string
	var watcherInterval string
	var podName string
	var skipCoreGrpcServerAuth string

	// Determine environment in which app is running.
	conf.RunningIn = determineEnvironment()
	conf.UtMode = utMode

	// Core gRPC config
	// For each env var, try the new CORE_GRPC_* name first then fall back to the legacy CARBIDE_* name.
	// TODO: remove CARBIDE_* fallbacks once deployment config repo is fully updated to CORE_GRPC_* vars.
	coreGrpcAddress := os.Getenv("CORE_GRPC_ADDRESS")
	if coreGrpcAddress == "" {
		coreGrpcAddress = os.Getenv("CARBIDE_ADDRESS")
	}
	flag.StringVar(&conf.CoreGrpc.Address, "coreGrpcAddress", coreGrpcAddress, "Core gRPC Server Address")
	if conf.CoreGrpc.Address == "" {
		conf.CoreGrpc.Address = DefaultCoreGrpcAddress
	}

	coreGrpcSecOptStr := os.Getenv("CORE_GRPC_SEC_OPT")
	if coreGrpcSecOptStr == "" {
		coreGrpcSecOptStr = os.Getenv("CARBIDE_SEC_OPT") // TODO: remove once deployment config repo is updated
	}
	coreGrpcSecOpt, err := strconv.Atoi(coreGrpcSecOptStr)
	if err != nil {
		log.Info().Err(err).Msg("Invalid Core gRPC security option, using server TLS as default")
		coreGrpcSecOpt = int(client.ServerTLS)
	}
	if coreGrpcSecOpt < int(client.InsecureGrpc) || coreGrpcSecOpt > int(client.MutualTLS) {
		coreGrpcSecOpt = int(client.ServerTLS)
	}
	flag.IntVar(&coreGrpcSecOpt, "coreGrpcSecureOptions", coreGrpcSecOpt, "Core gRPC security option")
	conf.CoreGrpc.Secure = client.SecureOptions(coreGrpcSecOpt)

	coreGrpcCACertPath := os.Getenv("CORE_GRPC_CA_CERT_PATH")
	if coreGrpcCACertPath == "" {
		coreGrpcCACertPath = os.Getenv("CARBIDE_CA_CERT_PATH") // TODO: remove once deployment config repo is updated
	}
	flag.StringVar(&conf.CoreGrpc.ServerCAPath, "coreGrpcCACertPath", coreGrpcCACertPath, "Core gRPC CA Cert Path")
	if conf.CoreGrpc.ServerCAPath == "" {
		conf.CoreGrpc.ServerCAPath = DefaultCoreGrpcCACertPath
	}
	coreGrpcClientCertPath := os.Getenv("CORE_GRPC_CLIENT_CERT_PATH")
	if coreGrpcClientCertPath == "" {
		coreGrpcClientCertPath = os.Getenv("CARBIDE_CLIENT_CERT_PATH") // TODO: remove once deployment config repo is updated
	}
	flag.StringVar(&conf.CoreGrpc.ClientCertPath, "coreGrpcClientCertPath", coreGrpcClientCertPath, "Core gRPC client Cert Path")
	if conf.CoreGrpc.ClientCertPath == "" {
		conf.CoreGrpc.ClientCertPath = DefaultCoreGrpcClientCertPath
	}
	coreGrpcClientKeyPath := os.Getenv("CORE_GRPC_CLIENT_KEY_PATH")
	if coreGrpcClientKeyPath == "" {
		coreGrpcClientKeyPath = os.Getenv("CARBIDE_CLIENT_KEY_PATH") // TODO: remove once deployment config repo is updated
	}
	flag.StringVar(&conf.CoreGrpc.ClientKeyPath, "coreGrpcClientKeyPath", coreGrpcClientKeyPath, "Core gRPC client Key Path")
	if conf.CoreGrpc.ClientKeyPath == "" {
		conf.CoreGrpc.ClientKeyPath = DefaultCoreGrpcClientKeyPath
	}

	log.Info().Msg("Core gRPC Address:" + conf.CoreGrpc.Address)
	log.Info().Msg("Core gRPC Secure Options:" + strconv.Itoa(int(conf.CoreGrpc.Secure)))
	log.Info().Msg("Core gRPC CA Cert Path:" + conf.CoreGrpc.ServerCAPath)
	log.Info().Msg("Core gRPC client Cert Path:" + conf.CoreGrpc.ClientCertPath)
	log.Info().Msg("Core gRPC client Key Path:" + conf.CoreGrpc.ClientKeyPath)

	// Flow config
	flowGrpcAddress := os.Getenv("FLOW_GRPC_ADDRESS")
	if flowGrpcAddress == "" {
		flowGrpcAddress = os.Getenv("FLOW_ADDRESS") // TODO: remove once deployment config repo is updated
	}
	flag.StringVar(&conf.FlowGrpc.Address, "flowGrpcAddress", flowGrpcAddress, "Flow gRPC Address")
	if conf.FlowGrpc.Address == "" {
		conf.FlowGrpc.Address = DefaultFlowGrpcAddress
	}

	flowGrpcSecOptStr := os.Getenv("FLOW_GRPC_SEC_OPT")
	if flowGrpcSecOptStr == "" {
		flowGrpcSecOptStr = os.Getenv("FLOW_SEC_OPT") // TODO: remove once deployment config repo is updated
	}
	flowGrpcSecOpt, err := strconv.Atoi(flowGrpcSecOptStr)
	if err != nil {
		log.Info().Err(err).Msg("Invalid Flow gRPC security option, using server TLS as default")
		flowGrpcSecOpt = int(client.FlowServerTLS)
	}
	if flowGrpcSecOpt < int(client.FlowInsecureGrpc) || flowGrpcSecOpt > int(client.FlowMutualTLS) {
		flowGrpcSecOpt = int(client.FlowServerTLS)
	}
	flag.IntVar(&flowGrpcSecOpt, "flowGrpcSecureOptions", flowGrpcSecOpt, "Flow gRPC security option")
	conf.FlowGrpc.Secure = client.FlowGrpcClientSecureOptions(flowGrpcSecOpt)

	flowGrpcCACertPath := os.Getenv("FLOW_GRPC_CA_CERT_PATH")
	if flowGrpcCACertPath == "" {
		flowGrpcCACertPath = os.Getenv("FLOW_CA_CERT_PATH") // TODO: remove once deployment config repo is updated
	}
	if flowGrpcCACertPath == "" {
		flowGrpcCACertPath = os.Getenv("CARBIDE_CA_CERT_PATH") // TODO: remove once deployment config repo is updated
	}
	flag.StringVar(&conf.FlowGrpc.ServerCAPath, "flowGrpcCACertPath", flowGrpcCACertPath, "Flow gRPC CA Cert Path")
	if conf.FlowGrpc.ServerCAPath == "" {
		conf.FlowGrpc.ServerCAPath = DefaultFlowGrpcCACertPath
	}

	flowGrpcClientCertPath := os.Getenv("FLOW_GRPC_CLIENT_CERT_PATH")
	if flowGrpcClientCertPath == "" {
		flowGrpcClientCertPath = os.Getenv("FLOW_CLIENT_CERT_PATH") // TODO: remove once deployment config repo is updated
	}
	if flowGrpcClientCertPath == "" {
		flowGrpcClientCertPath = os.Getenv("CARBIDE_CLIENT_CERT_PATH") // TODO: remove once deployment config repo is updated
	}
	flag.StringVar(&conf.FlowGrpc.ClientCertPath, "flowGrpcClientCertPath", flowGrpcClientCertPath, "Flow gRPC client Cert Path")
	if conf.FlowGrpc.ClientCertPath == "" {
		conf.FlowGrpc.ClientCertPath = DefaultFlowGrpcClientCertPath
	}

	flowGrpcClientKeyPath := os.Getenv("FLOW_GRPC_CLIENT_KEY_PATH")
	if flowGrpcClientKeyPath == "" {
		flowGrpcClientKeyPath = os.Getenv("FLOW_CLIENT_KEY_PATH") // TODO: remove once deployment config repo is updated
	}
	if flowGrpcClientKeyPath == "" {
		flowGrpcClientKeyPath = os.Getenv("CARBIDE_CLIENT_KEY_PATH") // TODO: remove once deployment config repo is updated
	}
	flag.StringVar(&conf.FlowGrpc.ClientKeyPath, "flowGrpcClientKeyPath", flowGrpcClientKeyPath, "Flow gRPC client Key Path")
	if conf.FlowGrpc.ClientKeyPath == "" {
		conf.FlowGrpc.ClientKeyPath = DefaultFlowGrpcClientKeyPath
	}

	log.Info().Msg("Flow gRPC Address:" + conf.FlowGrpc.Address)
	log.Info().Msg("Flow gRPC CA Cert Path:" + conf.FlowGrpc.ServerCAPath)
	log.Info().Msg("Flow gRPC client Cert Path:" + conf.FlowGrpc.ClientCertPath)
	log.Info().Msg("Flow gRPC client Key Path:" + conf.FlowGrpc.ClientKeyPath)

	// General config
	flag.StringVar(&conf.MetricsPort, "metricsPort", os.Getenv("METRICS_PORT"), "Metrics port number")
	flag.StringVar(&conf.MetricsNamespace, "metricsNamespace", os.Getenv("METRICS_NAMESPACE"), "Prefix applied to every exposed metric name")
	flag.StringVar(&conf.Temporal.Host, "temporalHost", os.Getenv("TEMPORAL_HOST"), "Temporal hostname/IP")
	flag.StringVar(&conf.Temporal.Port, "temporalPort", os.Getenv("TEMPORAL_PORT"), "Temporal port")
	flag.StringVar(&enableDebug, "enableDebug", os.Getenv("ENABLE_DEBUG"), "Debug log level setting")
	flag.StringVar(&devmode, "devMode", os.Getenv("DEV_MODE"), "Local development")
	flag.StringVar(&enableTLS, "enableTLS", os.Getenv("ENABLE_TLS"), "Enable TLS based auth")
	flag.StringVar(&disableBootstrap, "disableBootstrap", os.Getenv("DISABLE_BOOTSTRAP"), "Disable secret based bootstrap")
	flag.StringVar(&conf.BootstrapSecret, "bootstrapSecret", os.Getenv("BOOTSTRAP_SECRET"), "Bootstrap secret")
	flag.StringVar(&watcherInterval, "watcherInterval", os.Getenv("WATCHER_INTERVAL"), "Watcher Interval")
	flag.StringVar(&podName, "podName", os.Getenv("POD_NAME"), "POD Name")
	flag.StringVar(&conf.PodNamespace, "podNamespace", os.Getenv("POD_NAMESPACE"), "POD Namespace")
	flag.StringVar(&conf.TemporalSecret, "temporalSecret", os.Getenv("TEMPORAL_CERT"), "Temporal cert secret")
	flag.StringVar(&conf.CloudVersion, "cloudVersion", os.Getenv("CLOUD_WORKFLOW_VERSION"), "Cloud Workflow Proto version")
	flag.StringVar(&conf.SiteVersion, "siteVersion", os.Getenv("SITE_WORKFLOW_VERSION"), "Site Workflow Proto version")
	flag.StringVar(&skipCoreGrpcServerAuth, "skipCoreGrpcServerAuth", os.Getenv("SKIP_GRPC_SERVER_AUTH"), "Skip gRPC server auth in TLS")

	var skipFlowGrpcServerAuth string
	flag.StringVar(&skipFlowGrpcServerAuth, "flowGrpcSkipServerAuth", os.Getenv("SKIP_FLOW_GRPC_SERVER_AUTH"), "Skip Flow gRPC server auth in TLS")

	var flowGrpcEnabled string
	flag.StringVar(&flowGrpcEnabled, "flowGrpcEnabled", os.Getenv("FLOW_GRPC_ENABLED"), "Enable Flow gRPC")

	if conf.MetricsPort == "" {
		log.Fatal().Msg("error loading config, invalid metrics port")
	}
	if conf.Temporal.Host == "" {
		log.Fatal().Msg("error loading config, Temporal host must be specified")
	}
	if conf.Temporal.Port == "" {
		log.Fatal().Msg("error loading config, invalid Temporal port")
	}
	if podName == "" {
		log.Fatal().Msg("error loading config, empty Pod Name")
	} else {
		conf.IsMasterPod = false
		parts := regexp.MustCompile(`(.*)-(\d+)$`).FindStringSubmatch(podName)
		if len(parts) == 3 {
			id, err := strconv.Atoi(parts[2])
			if err != nil {
				log.Fatal().Msgf("error loading config, invalid Pod Name %v %v", podName, err.Error())
			}
			if id == 0 {
				conf.IsMasterPod = true
			}
		} else {
			log.Fatal().Msgf("error loading config, invalid Pod Name %v", podName)
		}
	}
	if conf.PodNamespace == "" {
		log.Fatal().Msg("error loading config, empty Pod Namespace")
	}

	conf.EnableDebug = strings.ToLower(enableDebug) == "true"
	conf.DevMode = strings.ToLower(devmode) == "true"
	conf.EnableTLS = strings.ToLower(enableTLS) == "true"
	conf.DisableBootstrap = strings.ToLower(disableBootstrap) == "true"
	conf.CoreGrpc.SkipServerAuth = strings.ToLower(skipCoreGrpcServerAuth) == "true"
	conf.FlowGrpc.SkipServerAuth = strings.ToLower(skipFlowGrpcServerAuth) == "true"
	conf.FlowGrpc.Enabled = strings.ToLower(flowGrpcEnabled) == "true"

	// Initialize the WatcherInterval to default if not defined
	if watcherInterval == "" {
		watcherInterval = "10"
	}
	wi, err := strconv.Atoi(watcherInterval)
	if err != nil {
		log.Fatal().Msg(fmt.Sprint("invalid watcher interval", err))
	}
	// convert watcherInterval to Minutes
	conf.WatcherInterval = time.Duration(wi) * time.Minute

	if conf.BootstrapSecret == "" {
		conf.BootstrapSecret = "/etc/sitereg/"
	}

	// Site ID
	// TODO: Rename CLUSTER_ID to SITE_ID
	clusterID := ""
	if csi := os.Getenv("CLUSTER_ID"); csi != "" {
		clusterID = csi
	}
	_, err = uuid.Parse(clusterID)
	if err != nil {
		log.Fatal().Msg("error loading config, specified Cluster ID is not a UUID")
	}

	// Load the Temporal configuration from env vars
	var temporalPublishQueue string
	if mcq := os.Getenv("TEMPORAL_PUBLISH_QUEUE"); mcq != "" {
		temporalPublishQueue = mcq
	}

	var temporalSubscribeQueue string
	if msq := os.Getenv("TEMPORAL_SUBSCRIBE_QUEUE"); msq != "" {
		temporalSubscribeQueue = msq
	}

	var temporalPublishNamespace string
	if mcq := os.Getenv("TEMPORAL_PUBLISH_NAMESPACE"); mcq != "" {
		temporalPublishNamespace = mcq
	}

	temporalSubscribeNamespace := clusterID
	if msq := os.Getenv("TEMPORAL_SUBSCRIBE_NAMESPACE"); msq != "" {
		temporalSubscribeNamespace = msq
	}

	temporalCertPath := ""
	if msf := os.Getenv("TEMPORAL_CERT_PATH"); msf != "" {
		temporalCertPath = msf
	}

	flag.StringVar(&conf.Temporal.TemporalPublishQueue, "temporalPublishQueue", temporalPublishQueue, "Temporal Publish queue")
	flag.StringVar(&conf.Temporal.TemporalSubscribeQueue, "temporalSubscribeQueue", temporalSubscribeQueue, "Temporal Subscribe queue")
	flag.StringVar(&conf.Temporal.TemporalPublishNamespace, "temporalPublishNamespace", temporalPublishNamespace, "Temporal Publish Namespace")
	flag.StringVar(&conf.Temporal.TemporalSubscribeNamespace, "temporalSubscribeNamespace", temporalSubscribeNamespace, "Temporal Subscribe Namespace")
	flag.StringVar(&conf.Temporal.ClusterID, "clusterID", clusterID, "NICo Site Cluster ID")
	flag.StringVar(&conf.Temporal.TemporalCertPath, "temporalCertPath", temporalCertPath, "Temporal cert path")
	flag.StringVar(&conf.Temporal.TemporalServer, "temporalServer", os.Getenv("TEMPORAL_SERVER"), "Temporal server")
	flag.StringVar(&conf.Temporal.TemporalInventorySchedule, "temporalInventorySchedule", os.Getenv("TEMPORAL_INVENTORY_SCHEDULE"), "Temporal Inventory schedule")

	inventoryCloudPageSize := conftypes.DefaultInventoryCloudPageSize
	if v := os.Getenv("INVENTORY_CLOUD_PAGE_SIZE"); v != "" {
		parsed, perr := strconv.Atoi(v)
		if perr != nil {
			log.Fatal().Msgf("error loading config, INVENTORY_CLOUD_PAGE_SIZE %q is not a valid integer", v)
		}
		inventoryCloudPageSize = parsed
	}
	flag.IntVar(&conf.Temporal.InventoryCloudPageSize, "inventoryCloudPageSize", inventoryCloudPageSize, "Number of inventory items published to Cloud per Temporal workflow page")

	// Must run before validation: flag.XxxVar sets the destination immediately, but a real
	// CLI flag only overwrites it here, so validating first would let a bad CLI value slip
	// through unchecked.
	flag.Parse()

	if conf.Temporal.TemporalPublishQueue == "" {
		log.Fatal().Msg("error loading config, Temporal publish queue must be specified")
	}

	if conf.Temporal.TemporalSubscribeQueue == "" {
		log.Fatal().Msg("error loading config, Temporal subscribe queue must be specified")
	}

	serr := validateInventorySchedule(conf.Temporal.TemporalInventorySchedule)
	if serr != nil {
		log.Fatal().Msgf("error loading config, %v", serr)
	}

	serr = validateInventoryCloudPageSize(conf.Temporal.InventoryCloudPageSize)
	if serr != nil {
		log.Fatal().Msgf("error loading config, %v", serr)
	}

	log.Info().Interface("config", conf).Msg("Config Manager: Config loaded")

	// Set default metrics namespace if not specified
	if conf.MetricsNamespace == "" {
		conf.MetricsNamespace = conftypes.DefaultMetricsNamespace
	}

	return conf
}

// validateInventorySchedule rejects an inventory schedule the rest of the system cannot honor.
// Cloud waits out the interval derived from this schedule before acting on an object, so a
// schedule slower than MaxInventoryReceiptInterval would hold off deletions and status updates
// long enough to destabilize it. This is the only place that bound is enforced. An empty
// schedule falls back to the built-in default, which is well inside it.
func validateInventorySchedule(schedule string) error {
	if schedule == "" {
		return nil
	}

	interval, err := swu.InventoryIntervalFromSchedule(schedule)
	if err != nil {
		return fmt.Errorf("Temporal inventory %w", err)
	}
	if interval > cutil.MaxInventoryReceiptInterval {
		return fmt.Errorf("Temporal inventory schedule %q collects every %v, which is slower than the %v maximum",
			schedule, interval, cutil.MaxInventoryReceiptInterval)
	}

	return nil
}

// validateInventoryCloudPageSize rejects a page size Temporal cannot carry. Must be >=1 and
// <=MaxInventoryCloudPageSize (2MB Temporal blob ceiling, see conftypes.go). The page size no
// longer has to divide the Core fetch page evenly: every inventory now publishes through the
// shared collector, which buffers items across Core pages instead of chunking each one on its
// own, so a page size like 30 no longer desyncs the paging totals.
func validateInventoryCloudPageSize(pageSize int) error {
	if pageSize < 1 {
		return fmt.Errorf("INVENTORY_CLOUD_PAGE_SIZE %d must be at least 1", pageSize)
	}
	if pageSize > conftypes.MaxInventoryCloudPageSize {
		return fmt.Errorf("INVENTORY_CLOUD_PAGE_SIZE %d exceeds the %d maximum", pageSize, conftypes.MaxInventoryCloudPageSize)
	}

	return nil
}

func determineEnvironment() conftypes.RunInEnvironment {
	// Check for env file presence at explicit location.
	_, err := os.Stat("../../config.env")
	if err != nil {
		log.Info().Msg("Config Manager: Could not find .env file, assuming Kubernetes environment")
		return conftypes.RunningInK8s
	}

	log.Info().Msg("Config Manager: Found .env file, assuming Docker environment")
	err = godotenv.Load("../../config.env")
	if err != nil {
		log.Info().Str("err", err.Error()).Msg("Config Manager: Failed to load .env file")
	} else {
		log.Info().Msg("Config Manager: Successfully loaded .env file")
	}

	return conftypes.RunningInDocker
}
