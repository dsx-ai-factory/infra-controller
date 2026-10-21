// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package flowgrpc

import (
	"sync"

	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/gogo/status"
	"github.com/rs/zerolog/log"
	"google.golang.org/grpc/codes"
)

// checkCertsOnce is a local variable to ensure the go routine for checking if the certificate has changed only gets
// kicked off once even if CreateGrpcClient gets called multiple times
var checkCertsOnce sync.Once

// CreateGrpcClient creates the Flow gRPC client, this is called at Site Agent startup and will be retried until it succeeds
func (flowgrpc *API) CreateGrpcClient() error {
	// Initialize contextual logger
	logger := log.With().Str("Method", "FlowGrpcClient.CreateGrpcClient").Logger()
	logger.Info().Msg("Loading Flow gRPC client configuration")

	// Initialize the Flow gRPC client configuration
	ManagerAccess.Data.EB.Managers.FlowGrpc.Client.Config = &client.FlowGrpcClientConfig{
		Address:        ManagerAccess.Conf.EB.FlowGrpc.Address,
		Secure:         ManagerAccess.Conf.EB.FlowGrpc.Secure,
		ServerCAPath:   ManagerAccess.Conf.EB.FlowGrpc.ServerCAPath,
		SkipServerAuth: ManagerAccess.Conf.EB.FlowGrpc.SkipServerAuth,
		ClientCertPath: ManagerAccess.Conf.EB.FlowGrpc.ClientCertPath,
		ClientKeyPath:  ManagerAccess.Conf.EB.FlowGrpc.ClientKeyPath,
		ClientMetrics:  makeGrpcClientMetrics(),
		OnRPCFinish:    flowgrpc.UpdateGrpcClientState,
	}
	logger.Info().Interface("GrpcConfig", ManagerAccess.Data.EB.Managers.FlowGrpc.Client.Config).Msg("Creating Flow gRPC client")

	// Get initial certificate MD5 hashes
	initialClientMD5, initialServerMD5, err := ManagerAccess.Data.EB.Managers.FlowGrpc.Client.GetInitialCertMD5()
	if err != nil {
		logger.Error().Err(err).Msg("Failed to get initial certificate MD5 hashes for Flow gRPC client certificates")
		ManagerAccess.Data.EB.Managers.FlowGrpc.State.HealthStatus.Store(uint64(computils.CompUnhealthy))
		return err
	}
	newClient, err := client.NewFlowGrpcClient(ManagerAccess.Data.EB.Managers.FlowGrpc.Client.Config)
	if err != nil {
		logger.Error().Err(err).Msg("Failed to initialize Flow gRPC client")
		ManagerAccess.Data.EB.Managers.FlowGrpc.State.HealthStatus.Store(uint64(computils.CompUnhealthy))
		return err
	}

	// Since this is initial creation, there's no old client to manage. SwapClient still used for consistency.
	_ = ManagerAccess.Data.EB.Managers.FlowGrpc.Client.SwapClient(newClient)
	logger.Info().Msg("Successfully created Flow gRPC client")

	// Start the certificate check and reload routine in a background goroutine
	checkCertsOnce.Do(func() {
		go ManagerAccess.Data.EB.Managers.FlowGrpc.Client.CheckAndReloadCerts(initialClientMD5, initialServerMD5)
		logger.Info().Msg("Started certificate reload routine for Flow gRPC client")
	})

	ManagerAccess.Data.EB.Managers.FlowGrpc.State.HealthStatus.Store(uint64(computils.CompHealthy))

	return nil
}

// GetGrpcClient gets the Flow gRPC client
func (flowgrpc *API) GetGrpcClient() *client.FlowGrpcClient {
	return ManagerAccess.Data.EB.Managers.FlowGrpc.GetClient()
}

// isGrpcUp checks if the Flow gRPC connection is functional
func isGrpcUp(c codes.Code) bool {
	switch c {
	case codes.Unavailable, codes.Unauthenticated:
		return false
	}
	return true
}

// UpdateGrpcClientState updates the Flow gRPC client state
func (flowgrpc *API) UpdateGrpcClientState(err error) {
	if err == nil {
		ManagerAccess.Data.EB.Managers.FlowGrpc.State.GrpcSucc.Inc()
		ManagerAccess.Data.EB.Managers.FlowGrpc.State.HealthStatus.Store(uint64(computils.CompHealthy))
		return
	}
	ManagerAccess.Data.EB.Managers.FlowGrpc.State.GrpcFail.Inc()
	ManagerAccess.Data.EB.Managers.FlowGrpc.State.Err.Store(err.Error())
	log.Error().Err(err).Msg("Flow gRPC: Failed to send request to server")
	st, ok := status.FromError(err)
	if ok {
		if !isGrpcUp(st.Code()) {
			ManagerAccess.Data.EB.Managers.FlowGrpc.State.HealthStatus.Store(uint64(computils.CompUnhealthy))
			log.Error().Err(err).Msg("Flow gRPC: Connection down")
		} else {
			log.Info().Msgf("Flow gRPC: Application error %v", st.Code())
		}
	}
}
