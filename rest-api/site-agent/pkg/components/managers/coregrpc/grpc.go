// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package coregrpc

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

// CreateGrpcClient creates the Core gRPC client, this is called at Site Agent startup and will be retried until it succeeds
func (coregrpc *API) CreateGrpcClient() error {
	// Initialize contextual logger
	logger := log.With().Str("Method", "CoreGrpcClient.CreateGrpcClient").Logger()
	logger.Info().Msg("Loading Core gRPC client configuration")

	// Initialize the Core gRPC client configuration
	ManagerAccess.Data.EB.Managers.CoreGrpc.Client.Config = &client.CoreGrpcClientConfig{
		Address:        ManagerAccess.Conf.EB.CoreGrpc.Address,
		Secure:         ManagerAccess.Conf.EB.CoreGrpc.Secure,
		ServerCAPath:   ManagerAccess.Conf.EB.CoreGrpc.ServerCAPath,
		SkipServerAuth: ManagerAccess.Conf.EB.CoreGrpc.SkipServerAuth,
		ClientCertPath: ManagerAccess.Conf.EB.CoreGrpc.ClientCertPath,
		ClientKeyPath:  ManagerAccess.Conf.EB.CoreGrpc.ClientKeyPath,
		ClientMetrics:  makeGrpcClientMetrics(),
		OnRPCFinish:    coregrpc.UpdateGrpcClientState,
	}

	logger.Info().Interface("GrpcConfig", ManagerAccess.Data.EB.Managers.CoreGrpc.Client.Config).Msg("Creating Core gRPC client")

	// Get initial certificate MD5 hashes
	initialClientMD5, initialServerMD5, err := ManagerAccess.Data.EB.Managers.CoreGrpc.Client.GetInitialCertMD5()
	if err != nil {
		logger.Error().Err(err).Msg("Failed to get initial certificate MD5 hashes for Core gRPC client certificates")
		ManagerAccess.Data.EB.Managers.CoreGrpc.State.HealthStatus.Store(uint64(computils.CompUnhealthy))
		return err
	}

	newClient, err := client.NewCoreGrpcClient(ManagerAccess.Data.EB.Managers.CoreGrpc.Client.Config)
	if err != nil {
		logger.Error().Err(err).Msg("Failed to initialize Core gRPC client")
		ManagerAccess.Data.EB.Managers.CoreGrpc.State.HealthStatus.Store(uint64(computils.CompUnhealthy))
		return err
	}

	// Since this is initial creation, there's no old client to manage. SwapClient still used for consistency.
	_ = ManagerAccess.Data.EB.Managers.CoreGrpc.Client.SwapClient(newClient)
	logger.Info().Msg("Successfully created Core gRPC client")

	// Start the certificate check and reload routine in a background goroutine
	checkCertsOnce.Do(func() {
		go ManagerAccess.Data.EB.Managers.CoreGrpc.Client.CheckAndReloadCerts(initialClientMD5, initialServerMD5)
		logger.Info().Msg("Started certificate reload routine for Core gRPC client")
	})

	ManagerAccess.Data.EB.Managers.CoreGrpc.State.HealthStatus.Store(uint64(computils.CompHealthy))

	return nil
}

// GetGrpcClient gets the Core gRPC client
func (coregrpc *API) GetGrpcClient() *client.CoreGrpcClient {
	return ManagerAccess.Data.EB.Managers.CoreGrpc.GetClient()
}

// isGrpcUp checks if the Core gRPC connection is functional
func isGrpcUp(c codes.Code) bool {
	switch c {
	case codes.Unavailable, codes.Unauthenticated:
		return false
	}
	return true
}

// UpdateGrpcClientState updates the Core gRPC client state
func (coregrpc *API) UpdateGrpcClientState(err error) {
	if err == nil {
		ManagerAccess.Data.EB.Managers.CoreGrpc.State.GrpcSucc.Inc()
		ManagerAccess.Data.EB.Managers.CoreGrpc.State.HealthStatus.Store(uint64(computils.CompHealthy))
		return
	}
	ManagerAccess.Data.EB.Managers.CoreGrpc.State.GrpcFail.Inc()
	ManagerAccess.Data.EB.Managers.CoreGrpc.State.Err.Store(err.Error())
	log.Error().Err(err).Msg("Core gRPC: Failed to send request to server")
	st, ok := status.FromError(err)
	if ok {
		if !isGrpcUp(st.Code()) {
			ManagerAccess.Data.EB.Managers.CoreGrpc.State.HealthStatus.Store(uint64(computils.CompUnhealthy))
			log.Error().Err(err).Msg("Core gRPC: Connection down")
		} else {
			log.Info().Msgf("Core gRPC: Application error %v", st.Code())
		}
	}
}
