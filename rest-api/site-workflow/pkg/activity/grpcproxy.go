// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"errors"
	"os"

	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/grpc/status"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
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
	// siteID binds managed envelopes to this site. It is only used as key
	// material by the explicitly enabled legacy-history reader.
	siteID string
}

// NewManageCoreProxy returns a new ManageCoreProxy bound to the Core gRPC client
// and the site ID used to validate protected request contexts.
func NewManageCoreProxy(coreGrpcClient *client.CoreGrpcAtomicClient, siteID string) ManageCoreProxy {
	return ManageCoreProxy{
		coreGrpcAtomicClient: coreGrpcClient,
		siteID:               siteID,
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
	return invokeGRPCProxyOnSite(ctx, grpcproxy.Core, "InvokeCoreGRPCOnSite", grpcClient, m.siteID, req)
}

// ManageFlowProxy is the activity wrapper for the generic Flow gRPC proxy.
type ManageFlowProxy struct {
	flowGrpcAtomicClient *client.FlowGrpcAtomicClient
	// siteID binds managed envelopes to this site. It is only used as key
	// material by the explicitly enabled legacy-history reader.
	siteID string
}

// NewManageFlowProxy returns a new ManageFlowProxy bound to the Flow gRPC
// client and the site ID used to validate protected request contexts.
func NewManageFlowProxy(flowGrpcClient *client.FlowGrpcAtomicClient, siteID string) ManageFlowProxy {
	return ManageFlowProxy{
		flowGrpcAtomicClient: flowGrpcClient,
		siteID:               siteID,
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
	return invokeGRPCProxyOnSite(ctx, grpcproxy.Flow, "InvokeFlowGRPCOnSite", grpcClient, m.siteID, req)
}

// InvokeCoreGRPCProtectedOnSite requires a managed envelope before dispatch.
func (m *ManageCoreProxy) InvokeCoreGRPCProtectedOnSite(ctx context.Context, req grpcproxy.Request) (grpcproxy.Response, error) {
	if req.ProtectedSecrets == nil {
		return grpcproxy.Response{}, grpcproxy.ErrInvalidProtection
	}
	return m.InvokeCoreGRPCOnSite(ctx, req)
}

// InvokeFlowGRPCProtectedOnSite requires a managed envelope before dispatch.
func (m *ManageFlowProxy) InvokeFlowGRPCProtectedOnSite(ctx context.Context, req grpcproxy.Request) (grpcproxy.Response, error) {
	if req.ProtectedSecrets == nil {
		return grpcproxy.Response{}, grpcproxy.ErrInvalidProtection
	}
	return m.InvokeFlowGRPCOnSite(ctx, req)
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
	siteID string,
	req grpcproxy.Request,
) (grpcproxy.Response, error) {
	logger := log.With().Str("Activity", activityName).Str("Method", req.FullMethod).Logger()
	logger.Info().Msg("Starting activity")

	reqJSON := req.RequestJSON
	var protector *grpcproxy.VaultProtector
	var binding grpcproxy.ProtectionContext
	if req.ProtectedSecrets != nil {
		if len(req.EncryptedSecrets) != 0 {
			return grpcproxy.Response{}, grpcproxy.ErrInvalidProtection
		}
		binding = grpcproxy.NewProtectionContext(siteID, backend, activity.GetInfo(ctx).WorkflowExecution.ID, req)
		var err error
		protector, err = grpcproxy.NewVaultProtectorFromEnvironment()
		if err != nil {
			return grpcproxy.Response{}, err
		}
		secretsJSON, err := protector.Open(ctx, binding, req.ProtectedSecrets)
		if err != nil {
			return grpcproxy.Response{}, err
		}
		reqJSON, err = grpcproxy.MergeSecrets(reqJSON, secretsJSON)
		if err != nil {
			return grpcproxy.Response{}, grpcproxy.ErrInvalidProtection
		}
	} else if len(req.EncryptedSecrets) > 0 {
		// Explicit migration-only permission; never used by new REST senders.
		if os.Getenv("GRPC_PROXY_ALLOW_LEGACY_SECRETS") != "true" {
			return grpcproxy.Response{}, grpcproxy.ErrInvalidProtection
		}
		secretsJSON, err := grpcproxy.OpenLegacySecrets(req.EncryptedSecrets, siteID)
		if err != nil {
			return grpcproxy.Response{}, err
		}
		merged, err := grpcproxy.MergeSecrets(reqJSON, secretsJSON)
		if err != nil {
			logger.Warn().Err(err).Msg("Failed to merge request secrets")
			return grpcproxy.Response{}, swe.WrapErr(err)
		}
		reqJSON = merged
	}

	respJSON, err := grpcClient.InvokeJSON(ctx, req.FullMethod, reqJSON)
	if err != nil {
		if protector != nil {
			// Backend status details, causes and log messages can echo secrets.
			// Retain the status classification without persisting their text.
			if errors.Is(err, client.ErrUnknownProxyMethod) {
				return grpcproxy.Response{}, temporal.NewNonRetryableApplicationError("Unsupported proxy method", swe.ErrTypeNICoUnimplemented, nil)
			}
			return grpcproxy.Response{}, swe.WrapErr(status.Error(status.Code(err), "Proxied gRPC call failed"))
		}
		logger.Warn().Err(err).Msgf("Failed to proxy %s gRPC call", backend.Label)
		// A missing local descriptor means the backend was never called. Preserve
		// that distinction before Temporal serialization loses the sentinel.
		if errors.Is(err, client.ErrUnknownProxyMethod) {
			return grpcproxy.Response{}, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeNICoUnimplemented, err)
		}
		return grpcproxy.Response{}, swe.WrapErr(err)
	}

	logger.Info().Msg("Completed activity")
	if protector != nil {
		sealed, err := protector.Seal(ctx, binding.Response(), respJSON)
		if err != nil {
			// The backend may already have mutated state. The proxy workflow's
			// no-retry policy must also apply to this failure.
			return grpcproxy.Response{}, err
		}
		return grpcproxy.Response{ProtectedResponse: sealed}, nil
	}
	return grpcproxy.Response{ResponseJSON: respJSON}, nil
}
