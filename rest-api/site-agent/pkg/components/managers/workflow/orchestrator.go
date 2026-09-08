// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"os"
	"time"

	"github.com/rs/zerolog"
	zlogadapter "logur.dev/adapter/zerolog"
	"logur.dev/logur"

	"go.opentelemetry.io/otel"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/contrib/opentelemetry"
	"go.temporal.io/sdk/interceptor"
	"go.temporal.io/sdk/worker"

	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
	swu "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/util"
)

// Orchestrator - Workflow Orchestrator
func Orchestrator() {
	defer computils.UpdateState(ManagerAccess.Data.EB)
	log := ManagerAccess.Data.EB.Log
	state := ManagerAccess.Data.EB.Managers.Workflow.State

	// Cleanup resources
	if ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker != nil {
		ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.Stop()
		ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker = nil
	}
	if ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher != nil {
		ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher.Close()
		ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher = nil
	}
	if ManagerAccess.Data.EB.Managers.Workflow.Temporal.Subscriber != nil {
		ManagerAccess.Data.EB.Managers.Workflow.Temporal.Subscriber.Close()
		ManagerAccess.Data.EB.Managers.Workflow.Temporal.Subscriber = nil
	}

	// keep track how many events we've seen.
	state.ConnectionAttempted.Inc()
	state.SetConnectionTime(time.Now().String())

	err := workflowOrchestrator()
	if err != nil {
		state.HealthStatus.Store(uint64(computils.CompUnhealthy))
		errMsg := err.Error()
		state.SetErr(errMsg)
		log.Error().Msg(errMsg)
	} else {
		// keep track how many succeeded.
		state.ConnectionSucc.Inc()
		state.HealthStatus.Store(uint64(computils.CompHealthy))
	}
}

// StartWorkflow - Workflow init function
func workflowOrchestrator() error {
	// Set the global handle here
	log := ManagerAccess.Data.EB.Log

	// Initialize Temporal client
	log.Info().Msg("Workflow: Creating Elektra site agent Temporal workflow orchestrator")

	var clientInterceptors []interceptor.ClientInterceptor
	var workerInterceptors []interceptor.WorkerInterceptor

	// otelErr, not err: `var err error` is declared further down.
	otelInterceptor, otelErr := opentelemetry.NewTracingInterceptor(
		opentelemetry.TracerOptions{TextMapPropagator: otel.GetTextMapPropagator()})
	if otelErr != nil {
		return fmt.Errorf("creating Temporal tracing interceptor: %w", otelErr)
	}
	clientInterceptors = append(clientInterceptors, otelInterceptor)
	workerInterceptors = append(workerInterceptors, otelInterceptor)

	// Create logger for temporal using
	// zero logger
	// This is optional
	// ManagerAccess.Data.EB.Managers.Workflow.Temporal.Logger = lg.NewTemporalLogger(log)

	var publishClientConnOptions client.ConnectionOptions
	var subscribeClientConnOptions client.ConnectionOptions

	if ManagerAccess.Conf.EB.EnableTLS {
		log.Info().Msg("Workflow: Creating Forge Cluster Temporal client with TLS enable")

		// TemporalCertPath should exist
		if ManagerAccess.Conf.EB.Temporal.TemporalCertPath == "" {
			log.Panic().Err(errors.New("unable to find temporal cert path")).Msg("Workflow: Unable to find temporal cert path")
		}

		// Load client cert
		// CACertPath
		fileName, TemporalCACertPath := ManagerAccess.Conf.EB.Temporal.GetTemporalCACertFilePath()
		TemporalCACertPath = TemporalCACertPath + fileName

		// ClientCertPath
		kpFileName, TemporalClientCertPath := ManagerAccess.Conf.EB.Temporal.GetTemporalClientCertFilePath()
		log.Info().Msgf("Workflow: Paths are client: %s, ca: %s", TemporalClientCertPath, TemporalCACertPath)
		clientcert, err := tls.LoadX509KeyPair(fmt.Sprintf("%v/%v", TemporalClientCertPath, kpFileName[0]),
			fmt.Sprintf("%v/%v", TemporalClientCertPath, kpFileName[1]))
		if err != nil {
			log.Error().Msg("Workflow: Unable to read client certificates")
			return err
		}

		// Load server cert
		caCert, err := os.ReadFile(TemporalCACertPath)
		if err != nil {
			log.Error().Msg("Workflow: Unable to read server certificates")
			return err
		}
		caCertPool := x509.NewCertPool()
		caCertPool.AppendCertsFromPEM(caCert)

		// provide tls cert option for publishing workflows
		publishClientConnOptions = client.ConnectionOptions{
			TLS: &tls.Config{
				Certificates: []tls.Certificate{clientcert},
				ServerName:   ManagerAccess.Conf.EB.Temporal.TemporalServer,
				RootCAs:      caCertPool,
			},
			KeepAliveTime:    10 * time.Second,
			KeepAliveTimeout: 60 * time.Second,
		}

		// provide tls cert option for subscribing workflows
		subscribeClientConnOptions = client.ConnectionOptions{
			TLS: &tls.Config{
				Certificates: []tls.Certificate{clientcert},
				ServerName:   ManagerAccess.Conf.EB.Temporal.TemporalServer,
				RootCAs:      caCertPool,
			},
			KeepAliveTime:    10 * time.Second,
			KeepAliveTimeout: 60 * time.Second,
		}
	}
	var err error
	// Initialize client for publish namespace
	tLogger := logur.LoggerToKV(zlogadapter.New(zerolog.New(os.Stderr)))

	log.Info().Msgf("Workflow: Connecting to Host %v, Port %v", ManagerAccess.Conf.EB.Temporal.Host, ManagerAccess.Conf.EB.Temporal.Port)
	clientOptions := client.Options{
		HostPort:          fmt.Sprintf("%s:%s", ManagerAccess.Conf.EB.Temporal.Host, ManagerAccess.Conf.EB.Temporal.Port),
		Namespace:         ManagerAccess.Conf.EB.Temporal.TemporalPublishNamespace,
		ConnectionOptions: publishClientConnOptions,
		DataConverter:     swu.NewTemporalDataConverter(),
		Interceptors:      clientInterceptors,
		Logger:            tLogger,
	}

	if ManagerAccess.Data.EB.Conf.UtMode {
		log.Info().Msg("Workflow: UT mode Temporal client")
		ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher, err = client.NewLazyClient(clientOptions)
	} else {
		ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher, err = client.Dial(clientOptions)
	}
	if err != nil {
		log.Error().Msg("Workflow: Failed to create Temporal client")
		return err
	}

	// Initialize client for subscribe namespace
	clientOptions = client.Options{
		HostPort:          fmt.Sprintf("%s:%s", ManagerAccess.Conf.EB.Temporal.Host, ManagerAccess.Conf.EB.Temporal.Port),
		Namespace:         ManagerAccess.Conf.EB.Temporal.TemporalSubscribeNamespace,
		ConnectionOptions: subscribeClientConnOptions,
		DataConverter:     swu.NewTemporalDataConverter(),
		Interceptors:      clientInterceptors,
		Logger:            tLogger,
	}

	if ManagerAccess.Data.EB.Conf.UtMode {
		log.Info().Msg("Workflow: UT mode Temporal client")
		ManagerAccess.Data.EB.Managers.Workflow.Temporal.Subscriber, err = client.NewLazyClient(clientOptions)
	} else {
		ManagerAccess.Data.EB.Managers.Workflow.Temporal.Subscriber, err = client.Dial(clientOptions)
	}
	if err != nil {
		log.Error().Msg("Workflow: Failed to create Temporal client")
		return err
	}

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker = worker.New(
		ManagerAccess.Data.EB.Managers.Workflow.Temporal.Subscriber,
		ManagerAccess.Conf.EB.Temporal.TemporalSubscribeQueue,
		worker.Options{
			Interceptors:        workerInterceptors,
			WorkflowPanicPolicy: worker.FailWorkflow,
		})
	log.Info().Msg("Workflow: Registering orchestrator workflows and activities for elektra cluster ")

	if ManagerAccess.Conf.EB.DevMode {
		log.Info().Msg("Workflow: Enabled orchestrator for development")
	} else {
		log.Info().Msg("Workflow: Enabled orchestrator for production")
	}

	// Register all manager flows here
	// TODO: all RegisterSubscriber calls return an error and we ignore them. Should we?
	err = ManagerAccess.API.Site.RegisterPublisher()
	if err != nil {
		return err
	}

	ManagerAccess.API.VPC.RegisterSubscriber()
	ManagerAccess.API.VPC.RegisterPublisher()

	ManagerAccess.API.VpcPrefix.RegisterSubscriber()
	ManagerAccess.API.VpcPrefix.RegisterPublisher()

	ManagerAccess.API.VpcPeering.RegisterSubscriber()
	ManagerAccess.API.VpcPeering.RegisterPublisher()

	ManagerAccess.API.Subnet.RegisterSubscriber()
	ManagerAccess.API.Subnet.RegisterPublisher()

	ManagerAccess.API.InfiniBandPartition.RegisterSubscriber()
	ManagerAccess.API.InfiniBandPartition.RegisterPublisher()

	ManagerAccess.API.SSHKeyGroup.RegisterSubscriber()
	ManagerAccess.API.SSHKeyGroup.RegisterPublisher()

	ManagerAccess.API.Machine.RegisterSubscriber()
	ManagerAccess.API.Machine.RegisterPublisher()

	ManagerAccess.API.Instance.RegisterSubscriber()
	ManagerAccess.API.Instance.RegisterPublisher()

	ManagerAccess.API.Bootstrap.RegisterSubscriber()

	ManagerAccess.API.Tenant.RegisterSubscriber()
	ManagerAccess.API.Tenant.RegisterPublisher()

	ManagerAccess.API.OperatingSystem.RegisterSubscriber()
	ManagerAccess.API.OperatingSystem.RegisterPublisher()

	ManagerAccess.API.MachineValidation.RegisterSubscriber()

	// Generic Core gRPC proxy: one workflow/activity for all proxied operations,
	// registered on the Core gRPC manager that owns the connection.
	ManagerAccess.API.CoreGrpc.RegisterSubscriber()

	ManagerAccess.API.InstanceType.RegisterSubscriber()
	ManagerAccess.API.InstanceType.RegisterPublisher()

	ManagerAccess.API.NetworkSecurityGroup.RegisterSubscriber()
	ManagerAccess.API.NetworkSecurityGroup.RegisterPublisher()

	ManagerAccess.API.ExpectedMachine.RegisterSubscriber()
	ManagerAccess.API.ExpectedMachine.RegisterPublisher()

	ManagerAccess.API.ExpectedPowerShelf.RegisterSubscriber()
	ManagerAccess.API.ExpectedPowerShelf.RegisterPublisher()

	ManagerAccess.API.ExpectedRack.RegisterSubscriber()
	ManagerAccess.API.ExpectedRack.RegisterPublisher()

	ManagerAccess.API.ExpectedSwitch.RegisterSubscriber()
	ManagerAccess.API.ExpectedSwitch.RegisterPublisher()

	ManagerAccess.API.SKU.RegisterSubscriber()
	ManagerAccess.API.SKU.RegisterPublisher()

	ManagerAccess.API.DpuExtensionService.RegisterSubscriber()
	ManagerAccess.API.DpuExtensionService.RegisterPublisher()

	ManagerAccess.API.NVLinkLogicalPartition.RegisterSubscriber()
	ManagerAccess.API.NVLinkLogicalPartition.RegisterPublisher()

	ManagerAccess.API.TenantIdentity.RegisterSubscriber()

	// Flow workflows (only registered if Flow gRPC is enabled)
	if ManagerAccess.Conf.EB.FlowGrpc.Enabled {
		if ManagerAccess.API.FlowGrpc != nil {
			ManagerAccess.API.FlowGrpc.RegisterSubscriber()
		} else {
			log.Error().Msg("FlowGrpc: Flow gRPC is enabled in config but Flow gRPC manager is not initialized")
		}
	}

	// Start listening to the Task Queue
	log.Info().Msg("Workflow: Starting Temporal worker")
	err = ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.Start()
	if err != nil {
		log.Error().Msg("Workflow: Failed to start orchestrator worker")
		return err
	}

	return nil
}
