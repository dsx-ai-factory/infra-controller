// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"context"
	"fmt"
	"net/http"
	"os"
	"time"

	"github.com/getsentry/sentry-go"
	sentryZerolog "github.com/getsentry/sentry-go/zerolog"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	zlogadapter "logur.dev/adapter/zerolog"
	"logur.dev/logur"

	tsdkClient "go.temporal.io/sdk/client"
	tsdkConverter "go.temporal.io/sdk/converter"
	tsdkWorker "go.temporal.io/sdk/worker"

	"go.opentelemetry.io/otel"
	"go.temporal.io/sdk/contrib/opentelemetry"
	"go.temporal.io/sdk/interceptor"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/collectors"
	"github.com/prometheus/client_golang/prometheus/promhttp"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"

	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	cwfh "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/health"
	cwfn "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/namespace"

	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"

	machineActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/machine"
	machineWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/machine"

	vpcActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/vpc"
	vpcWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/vpc"

	subnetActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/subnet"
	subnetWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/subnet"

	instanceActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/instance"
	instanceWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/instance"

	userActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/user"
	userWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/user"

	siteActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/site"
	siteWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/site"

	sshKeyGroupActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/sshkeygroup"
	sshKeyGroupWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/sshkeygroup"

	ibpActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/infinibandpartition"
	ibpWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/infinibandpartition"

	expectedMachineActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/expectedmachine"
	expectedMachineWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/expectedmachine"

	expectedPowerShelfActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/expectedpowershelf"
	expectedPowerShelfWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/expectedpowershelf"

	expectedRackActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/expectedrack"
	expectedRackWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/expectedrack"

	expectedSwitchActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/expectedswitch"
	expectedSwitchWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/expectedswitch"

	tenantActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/tenant"
	tenantWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/tenant"

	instanceTypeActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/instancetype"
	instanceTypeWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/instancetype"

	networkSecurityGroupActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/networksecuritygroup"
	networkSecurityGroupWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/networksecuritygroup"

	osImageActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/operatingsystem"
	osImageWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/operatingsystem"

	ipxeTemplateActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/ipxetemplate"
	ipxeTemplateWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/ipxetemplate"

	skuActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/sku"
	skuWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/sku"

	vpcPrefixActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/vpcprefix"
	vpcPrefixWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/vpcprefix"

	vpcPeeringActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/vpcpeering"
	vpcPeeringWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/vpcpeering"

	dpuExtensionServiceActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/dpuextensionservice"
	dpuExtensionServiceWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/dpuextensionservice"

	nvLinkLogicalPartitionActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/nvlinklogicalpartition"
	nvLinkLogicalPartitionWorkflow "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/nvlinklogicalpartition"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/tracing"
)

const (
	// ZerologMessageFieldName specifies the field name for log message
	ZerologMessageFieldName = "msg"
	// ZerologLevelFieldName specifies the field name for log level
	ZerologLevelFieldName = "type"
)

func main() {
	// First: interceptors and handlers below capture the global propagator.
	tracing.InstallPropagator()
	// No-op unless OTEL_EXPORTER_OTLP_ENDPOINT is set.
	defer tracing.InstallExporter("nico-rest-workflow")()
	// Initialize context
	ctx := context.Background()

	// Initialize logger
	zerolog.TimeFieldFormat = zerolog.TimeFormatUnix
	zerolog.LevelFieldName = ZerologLevelFieldName
	zerolog.MessageFieldName = ZerologMessageFieldName

	cfg := config.NewConfig()
	defer cfg.Close()

	dbConfig := cfg.GetDBConfig()

	// Initialize DB connection
	dbSession, err := cdb.NewSession(ctx, dbConfig.Host, dbConfig.Port, dbConfig.Name, dbConfig.User, dbConfig.Password, "")
	if err != nil {
		log.Panic().Err(err).Msg("failed to initialize DB session")
	} else {
		defer dbSession.Close()
	}

	// Initializer Temporal client
	// Create the client object just once per process
	log.Info().Msg("creating Temporal client")

	// set up sentry client
	sentryDSN := os.Getenv("SENTRY_DSN")
	if sentryDSN != "" {
		// Initialize Sentry
		err := sentry.Init(sentry.ClientOptions{
			Dsn: sentryDSN,
			BeforeSend: func(event *sentry.Event, hint *sentry.EventHint) *sentry.Event {
				// Modify or filter events before sending them to Sentry
				return event
			},
			Debug:            true,
			AttachStacktrace: true,
		})
		if err != nil {
			log.Error().Err(err).Msg("Sentry initialization failed")
		} else {
			defer sentry.Flush(2 * time.Second)

			// Configure Zerolog to use Sentry as a writer
			sentryWriter, err := sentryZerolog.New(sentryZerolog.Config{
				ClientOptions: sentry.ClientOptions{
					Dsn: sentryDSN,
				},
				Options: sentryZerolog.Options{
					Levels:          []zerolog.Level{zerolog.ErrorLevel, zerolog.FatalLevel, zerolog.PanicLevel},
					WithBreadcrumbs: true,
					FlushTimeout:    3 * time.Second,
				},
			})
			if err != nil {
				log.Error().Err(err).Msg("failed to create Sentry writer")
			} else {
				defer sentryWriter.Close()

				// Use Sentry writer in Zerolog
				log.Logger = zerolog.New(zerolog.MultiLevelWriter(os.Stderr, sentryWriter))
			}
		}
	}

	tLogger := logur.LoggerToKV(zlogadapter.New(zerolog.New(os.Stderr)))
	var tc tsdkClient.Client

	tcfg, err := cfg.GetTemporalConfig()
	if err != nil {
		log.Panic().Err(err).Msg("failed to get Temporal config")
	}

	var tInterceptors []interceptor.ClientInterceptor
	var wInterceptors []interceptor.WorkerInterceptor

	if cfg.GetTracingEnabled() {
		otelInterceptor, err := opentelemetry.NewTracingInterceptor(opentelemetry.TracerOptions{TextMapPropagator: otel.GetTextMapPropagator()})
		if err != nil {
			log.Panic().Err(err).Msg("unable to get otelInterceptor")
		}
		tInterceptors = append(tInterceptors, otelInterceptor)
		wInterceptors = append(wInterceptors, otelInterceptor)
	}

	tc, err = tsdkClient.NewLazyClient(tsdkClient.Options{
		HostPort:  fmt.Sprintf("%v:%v", tcfg.Host, tcfg.Port),
		Namespace: tcfg.Namespace,
		ConnectionOptions: tsdkClient.ConnectionOptions{
			TLS: tcfg.ClientTLSCfg,
		},
		DataConverter: tsdkConverter.NewCompositeDataConverter(
			tsdkConverter.NewNilPayloadConverter(),
			tsdkConverter.NewByteSlicePayloadConverter(),
			tsdkConverter.NewProtoJSONPayloadConverterWithOptions(tsdkConverter.ProtoJSONPayloadConverterOptions{
				AllowUnknownFields: true,
			}),
			tsdkConverter.NewProtoPayloadConverter(),
			tsdkConverter.NewJSONPayloadConverter(),
		),
		Interceptors: tInterceptors,
		Logger:       tLogger,
	})

	if err != nil {
		log.Panic().Err(err).Msg("failed to create Temporal client")
	} else {
		defer tc.Close()
	}

	w := tsdkWorker.New(tc, tcfg.Queue, tsdkWorker.Options{
		WorkflowPanicPolicy:              tsdkWorker.FailWorkflow,
		MaxConcurrentActivityTaskPollers: 10,
		MaxConcurrentWorkflowTaskPollers: 10,
		Interceptors:                     wInterceptors,
	})

	siteClientPool := sc.NewClientPool(tcfg)

	log.Info().Str("Temporal Namespace", tcfg.Namespace).Msg("registering workflow and activities")

	// Register workflows
	if tcfg.Namespace == cwfn.CloudNamespace {
		// Workflows triggered by Cloud services
		w.RegisterWorkflow(vpcWorkflow.DeleteVpcByID)

		// Subnet workflows
		w.RegisterWorkflow(subnetWorkflow.DeleteSubnetByID)

		// Instance workflows
		w.RegisterWorkflow(instanceWorkflow.DeleteInstanceByID)
		w.RegisterWorkflow(instanceWorkflow.RebootInstanceByID)

		// User workflows
		w.RegisterWorkflow(userWorkflow.UpdateUserFromNGC)
		w.RegisterWorkflow(userWorkflow.UpdateUserFromNGCWithAuxiliaryID)

		// Site workflows
		w.RegisterWorkflow(siteWorkflow.DeleteSiteComponents)
		w.RegisterWorkflow(siteWorkflow.MonitorHealthForAllSites)
		w.RegisterWorkflow(siteWorkflow.MonitorTemporalCertExpirationForAllSites)
		w.RegisterWorkflow(siteWorkflow.MonitorSiteTemporalNamespaces)

		// SSHKeyGroup workflows
		w.RegisterWorkflow(sshKeyGroupWorkflow.SyncSSHKeyGroup)
		w.RegisterWorkflow(sshKeyGroupWorkflow.DeleteSSHKeyGroup)

		// InfiniBandPartition workflows
		w.RegisterWorkflow(ibpWorkflow.DeleteInfiniBandPartitionByID)
	} else if tcfg.Namespace == cwfn.SiteNamespace {
		// Workflows triggered by Site Agent
		// Machine Workflows
		w.RegisterWorkflow(machineWorkflow.UpdateMachineInventory)

		// VPC workflows
		w.RegisterWorkflow(vpcWorkflow.UpdateVpcInventory)

		// Subnet workflows
		w.RegisterWorkflow(subnetWorkflow.UpdateSubnetInventory)

		// Instance workflows
		w.RegisterWorkflow(instanceWorkflow.UpdateInstanceInventory)

		// Site workflows
		w.RegisterWorkflow(siteWorkflow.UpdateAgentCertExpiry)
		// V1 stays registered for the rollout window, where Cloud upgrades ahead of the Site
		// Agents still publishing it.
		w.RegisterWorkflow(siteWorkflow.UpdateSiteConfigInventory)
		w.RegisterWorkflow(siteWorkflow.UpdateSiteConfigInventoryV2)

		// SSHKeyGroup workflows
		w.RegisterWorkflow(sshKeyGroupWorkflow.UpdateSSHKeyGroupInventory)

		// InfiniBandPartition workflows
		w.RegisterWorkflow(ibpWorkflow.UpdateInfiniBandPartitionInventory)

		// Tenant workflow
		w.RegisterWorkflow(tenantWorkflow.UpdateTenantInventory)

		// InstanceType workflow
		w.RegisterWorkflow(instanceTypeWorkflow.UpdateInstanceTypeInventory)

		// NetworkSecurityGroup workflow
		w.RegisterWorkflow(networkSecurityGroupWorkflow.UpdateNetworkSecurityGroupInventory)

		// OS Image workflow
		w.RegisterWorkflow(osImageWorkflow.UpdateOsImageInventory)

		// Operating System inventory workflow (inbound reconcile from nico-core)
		w.RegisterWorkflow(osImageWorkflow.UpdateOperatingSystemInventory)

		// iPXE Template inventory workflow
		w.RegisterWorkflow(ipxeTemplateWorkflow.UpdateIpxeTemplateInventory)

		// VPC Prefix workflow
		w.RegisterWorkflow(vpcPrefixWorkflow.UpdateVpcPrefixInventory)

		// VPC Peering workflow
		w.RegisterWorkflow(vpcPeeringWorkflow.UpdateVpcPeeringInventory)

		// ExpectedMachine workflow
		w.RegisterWorkflow(expectedMachineWorkflow.UpdateExpectedMachineInventory)

		// ExpectedPowerShelf workflow
		w.RegisterWorkflow(expectedPowerShelfWorkflow.UpdateExpectedPowerShelfInventory)

		// ExpectedRack workflow
		w.RegisterWorkflow(expectedRackWorkflow.UpdateExpectedRackInventory)

		// ExpectedSwitch workflow
		w.RegisterWorkflow(expectedSwitchWorkflow.UpdateExpectedSwitchInventory)

		// SKU workflow
		w.RegisterWorkflow(skuWorkflow.UpdateSkuInventory)

		// DPU Extension Service workflow
		w.RegisterWorkflow(dpuExtensionServiceWorkflow.UpdateDpuExtensionServiceInventory)

		// NVLink Logical Partition workflow
		w.RegisterWorkflow(nvLinkLogicalPartitionWorkflow.UpdateNVLinkLogicalPartitionInventory)
	}

	// Metric setup has to precede the activity registrations below, because the
	// activities hold their own metric handles and the worker will not accept a
	// registration once it is running. Only serving can wait for the goroutine
	// further down.
	mconfig := cfg.GetMetricsConfig()

	var reg *prometheus.Registry
	var siteHealthMetrics *cwm.SiteHealthMetrics

	if mconfig.Enabled {
		reg = prometheus.NewRegistry()
		reg.MustRegister(collectors.NewGoCollector())

		// Register core metrics
		cm := cwm.NewCoreMetrics(reg, mconfig.Namespace)
		// TODO: Set version here when available
		cm.Info.With(prometheus.Labels{"version": "unknown", "namespace": tcfg.Namespace}).Set(1)

		// Published by the Site health monitor cron, which runs on the Cloud queue.
		siteHealthMetrics = cwm.NewSiteHealthMetrics(reg, mconfig.Namespace)

		if tcfg.Namespace == cwfn.SiteNamespace {
			// The inventory workflows that report these metrics only run here.

			// Register common inventory metrics activity
			inventoryMetricsManager := cwm.NewManageInventoryMetrics(reg, dbSession, mconfig.Namespace)
			w.RegisterActivity(inventoryMetricsManager)

			// Register inventory operation metrics activity
			vpcLifecycleMetricsManager := vpcActivity.NewManageVpcLifecycleMetrics(reg, dbSession, mconfig.Namespace)
			w.RegisterActivity(&vpcLifecycleMetricsManager)

			subnetLifecycleMetricsManager := subnetActivity.NewManageSubnetLifecycleMetrics(reg, dbSession, mconfig.Namespace)
			w.RegisterActivity(&subnetLifecycleMetricsManager)

			instanceLifecycleMetricsManager := instanceActivity.NewManageInstanceLifecycleMetrics(reg, dbSession, mconfig.Namespace)
			w.RegisterActivity(&instanceLifecycleMetricsManager)
		}
	}

	// Register activities
	// Common activities
	machineManager := machineActivity.NewManageMachine(dbSession, siteClientPool)
	w.RegisterActivity(&machineManager)

	vpcManager := vpcActivity.NewManageVpc(dbSession, siteClientPool, tc)
	w.RegisterActivity(&vpcManager)

	subnetManager := subnetActivity.NewManageSubnet(dbSession, siteClientPool, tc)
	w.RegisterActivity(&subnetManager)

	instanceManager := instanceActivity.NewManageInstance(dbSession, siteClientPool, tc, cfg)
	w.RegisterActivity(&instanceManager)

	siteManager := siteActivity.NewManageSite(dbSession, siteClientPool, tc, cfg, siteHealthMetrics)
	w.RegisterActivity(&siteManager)

	sshKeyGroupManager := sshKeyGroupActivity.NewManageSSHKeyGroup(dbSession, siteClientPool)
	w.RegisterActivity(&sshKeyGroupManager)

	ibpManager := ibpActivity.NewManageInfiniBandPartition(dbSession, siteClientPool)
	w.RegisterActivity(&ibpManager)

	tenantManager := tenantActivity.NewManageTenant(dbSession, siteClientPool)
	w.RegisterActivity(&tenantManager)

	instanceTypeManager := instanceTypeActivity.NewManageInstanceType(dbSession, siteClientPool)
	w.RegisterActivity(&instanceTypeManager)

	networkSecurityGroupManager := networkSecurityGroupActivity.NewManageNetworkSecurityGroup(dbSession, siteClientPool)
	w.RegisterActivity(&networkSecurityGroupManager)

	osImageManager := osImageActivity.NewManageOsImage(dbSession, siteClientPool)
	w.RegisterActivity(&osImageManager)

	ipxeTemplateManager := ipxeTemplateActivity.NewManageIpxeTemplate(dbSession, siteClientPool)
	w.RegisterActivity(&ipxeTemplateManager)

	vpcPrefixManager := vpcPrefixActivity.NewManageVpcPrefix(dbSession, siteClientPool)
	w.RegisterActivity(&vpcPrefixManager)

	vpcPeeringManager := vpcPeeringActivity.NewManageVpcPeering(dbSession, siteClientPool)
	w.RegisterActivity(&vpcPeeringManager)

	// ExpectedMachine activities
	expectedMachineManager := expectedMachineActivity.NewManageExpectedMachine(dbSession, siteClientPool)
	w.RegisterActivity(&expectedMachineManager)

	// ExpectedPowerShelf activities
	expectedPowerShelfManager := expectedPowerShelfActivity.NewManageExpectedPowerShelf(dbSession, siteClientPool)
	w.RegisterActivity(&expectedPowerShelfManager)

	// ExpectedRack activities
	expectedRackManager := expectedRackActivity.NewManageExpectedRack(dbSession, siteClientPool)
	w.RegisterActivity(&expectedRackManager)

	// ExpectedSwitch activities
	expectedSwitchManager := expectedSwitchActivity.NewManageExpectedSwitch(dbSession, siteClientPool)
	w.RegisterActivity(&expectedSwitchManager)

	// SKU activities
	skuManager := skuActivity.NewManageSku(dbSession, siteClientPool)
	w.RegisterActivity(&skuManager)

	// DPU Extension Service activities
	dpuExtensionServiceManager := dpuExtensionServiceActivity.NewManageDpuExtensionService(dbSession, siteClientPool)
	w.RegisterActivity(&dpuExtensionServiceManager)

	// NVLink Logical Partition activities
	nvLinkLogicalPartitionManager := nvLinkLogicalPartitionActivity.NewManageNVLinkLogicalPartition(dbSession, siteClientPool)
	w.RegisterActivity(&nvLinkLogicalPartitionManager)

	if tcfg.Namespace == cwfn.CloudNamespace {
		// User activities
		userManager := userActivity.NewManageUser(dbSession, cfg)
		w.RegisterActivity(&userManager)
	}

	// Serve health endpoint
	hconfig := cfg.GetHealthzConfig()
	if hconfig.Enabled {
		go func() {
			log.Info().Msg("starting health check API server")
			http.HandleFunc("/healthz", cwfh.StatusHandler)
			http.HandleFunc("/readyz", cwfh.StatusHandler)

			serr := http.ListenAndServe(hconfig.GetListenAddr(), nil)
			if serr != nil {
				log.Panic().Err(serr).Msg("failed to start health check server")
			}
		}()
	}

	if mconfig.Enabled {
		// Serve Prometheus metrics
		go func() {
			log.Info().Msg("starting Prometheus metrics server")

			promHandler := promhttp.HandlerFor(reg, promhttp.HandlerOpts{Registry: reg})

			http.Handle("/metrics", promHandler)
			serr := http.ListenAndServe(mconfig.GetListenAddr(), nil)
			if serr != nil {
				log.Panic().Err(serr).Msg("failed to start Prometheus metrics server")
			}
		}()
	}

	// Start listening to the Task Queue
	log.Info().Str("Temporal Namespace", tcfg.Namespace).Msg("starting Temporal worker")
	err = w.Run(tsdkWorker.InterruptCh())
	if err != nil {
		log.Panic().Err(err).Str("Temporal Namespace", tcfg.Namespace).Msg("failed to start worker")
	}

	// Trigger cron workflow
	if tcfg.Namespace == cwfn.CloudNamespace {
		_, err := siteWorkflow.ExecuteMonitorHealthForAllSitesWorkflow(ctx, tc)
		if err != nil {
			log.Error().Err(err).Msg("failed to trigger Site Health Monitor workflow")
		}

		// Trigger MonitorTemporalCertExpirationForAllSites
		_, err = siteWorkflow.ExecuteMonitorTemporalCertExpirationForAllSites(ctx, tc)
		if err != nil {
			log.Error().Err(err).Msg("failed to trigger Temporal Cert Expiration Monitor workflow")
		}

		// Trigger MonitorSiteTemporalNamespaces
		_, err = siteWorkflow.ExecuteMonitorSiteTemporalNamespaces(ctx, tc)
		if err != nil {
			log.Error().Err(err).Msg("failed to trigger Monitor Site Temporal Namespaces workflow")
		}
	}

	// NOTE: Log messages past this point do not show up in the log output
}
