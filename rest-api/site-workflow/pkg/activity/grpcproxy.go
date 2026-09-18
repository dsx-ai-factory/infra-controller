// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"errors"

	"go.temporal.io/sdk/temporal"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cloudutils "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/rs/zerolog/log"
)

// jsonInvoker is the connected backend client the proxy activity calls. Both
// CoreGrpcClient and FlowGrpcClient satisfy it.
type jsonInvoker interface {
	InvokeJSON(ctx context.Context, method string, reqJSON []byte) ([]byte, error)
}

// ManageCoreProxy is the activity wrapper for the generic NICo Core gRPC proxy.
type ManageCoreProxy struct {
	coreGrpcAtomicClient *client.CoreGrpcAtomicClient
	// secretKey decrypts the redacted secret fields carried in
	// grpcproxy.Request.EncryptedSecrets. It is the shared site key (the
	// site/cluster ID), matching the key the cloud used to encrypt them.
	secretKey string
}

// NewManageCoreProxy returns a new ManageCoreProxy bound to the Core gRPC client
// and the site secret key used to decrypt redacted request fields.
func NewManageCoreProxy(coreGrpcClient *client.CoreGrpcAtomicClient, secretKey string) ManageCoreProxy {
	return ManageCoreProxy{
		coreGrpcAtomicClient: coreGrpcClient,
		secretKey:            secretKey,
	}
}

// InvokeCoreGRPCOnSite proxies a single Core gRPC call described by req.
//
// The method name is the Temporal activity type, so it must stay stable even
// though the body is shared with the Flow proxy.
func (m *ManageCoreProxy) InvokeCoreGRPCOnSite(ctx context.Context, req grpcproxy.Request) (grpcproxy.Response, error) {
	grpcClient := m.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return grpcproxy.Response{}, client.ErrCoreGrpcClientNotConnected
	}
	return invokeGRPCProxyOnSite(ctx, grpcproxy.Core, "InvokeCoreGRPCOnSite", grpcClient, m.secretKey, req)
}

// ManageFlowProxy is the activity wrapper for the generic Flow gRPC proxy.
type ManageFlowProxy struct {
	flowGrpcAtomicClient *client.FlowGrpcAtomicClient
	// secretKey decrypts the redacted secret fields carried in
	// grpcproxy.Request.EncryptedSecrets. It is the shared site key (the
	// site/cluster ID), matching the key the cloud used to encrypt them.
	secretKey string
}

// NewManageFlowProxy returns a new ManageFlowProxy bound to the Flow gRPC
// client and the site secret key used to decrypt redacted request fields.
func NewManageFlowProxy(flowGrpcClient *client.FlowGrpcAtomicClient, secretKey string) ManageFlowProxy {
	return ManageFlowProxy{
		flowGrpcAtomicClient: flowGrpcClient,
		secretKey:            secretKey,
	}
}

// InvokeFlowGRPCOnSite proxies a single Flow gRPC call described by req.
//
// The method name is the Temporal activity type, so it must stay stable even
// though the body is shared with the Core proxy.
func (m *ManageFlowProxy) InvokeFlowGRPCOnSite(ctx context.Context, req grpcproxy.Request) (grpcproxy.Response, error) {
	grpcClient := m.flowGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return grpcproxy.Response{}, client.ErrFlowGrpcClientNotConnected
	}
	return invokeGRPCProxyOnSite(ctx, grpcproxy.Flow, "InvokeFlowGRPCOnSite", grpcClient, m.secretKey, req)
}

// invokeGRPCProxyOnSite performs one proxied call against an already-connected
// backend. Any redacted secret fields are decrypted and merged back into the
// request before it leaves the site agent. The request body is intentionally
// never logged because it may contain secrets (e.g. BMC credential passwords);
// only the method is.
func invokeGRPCProxyOnSite(
	ctx context.Context,
	backend grpcproxy.Backend,
	activityName string,
	grpcClient jsonInvoker,
	secretKey string,
	req grpcproxy.Request,
) (grpcproxy.Response, error) {
	logger := log.With().Str("Activity", activityName).Str("Method", req.FullMethod).Logger()
	logger.Info().Msg("Starting activity")

	reqJSON := req.RequestJSON
	if len(req.EncryptedSecrets) > 0 {
		secretsJSON := cloudutils.DecryptData(req.EncryptedSecrets, secretKey)
		merged, err := grpcproxy.MergeSecrets(reqJSON, secretsJSON)
		if err != nil {
			logger.Warn().Err(err).Msg("Failed to merge request secrets")
			return grpcproxy.Response{}, swe.WrapErr(err)
		}
		reqJSON = merged
	}

	respJSON, err := grpcClient.InvokeJSON(ctx, req.FullMethod, reqJSON)
	if err != nil {
		logger.Warn().Err(err).Msgf("Failed to proxy %s gRPC call", backend.Label)
		// A missing local descriptor means the backend was never called. Preserve
		// that distinction before Temporal serialization loses the sentinel.
		if errors.Is(err, client.ErrUnknownProxyMethod) {
			return grpcproxy.Response{}, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeNICoUnimplemented, err)
		}
		return grpcproxy.Response{}, swe.WrapErr(err)
	}

	logger.Info().Msg("Completed activity")
	return grpcproxy.Response{ResponseJSON: respJSON}, nil
}
