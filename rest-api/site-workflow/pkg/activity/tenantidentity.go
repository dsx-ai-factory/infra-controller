// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"errors"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/types/known/emptypb"
)

// ManageTenantIdentity wraps the tenant-identity activities.
type ManageTenantIdentity struct {
	CoreGrpcAtomicClient *client.CoreGrpcAtomicClient
}

// NewManageTenantIdentity returns a new ManageTenantIdentity activity manager.
func NewManageTenantIdentity(carbideClient *client.CoreGrpcAtomicClient) ManageTenantIdentity {
	return ManageTenantIdentity{
		CoreGrpcAtomicClient: carbideClient,
	}
}

// CreateOrUpdateTenantIdentityConfigurationOnSite is an activity to create or update Tenant Identity Config using Core gRPC API
func (m *ManageTenantIdentity) CreateOrUpdateTenantIdentityConfigurationOnSite(
	ctx context.Context,
	request *corev1.SetTenantIdentityConfigRequest,
) (*corev1.TenantIdentityConfigResponse, error) {
	logger := log.With().Str("Activity", "CreateOrUpdateTenantIdentityConfigurationOnSite").Logger()
	logger.Info().Msg("Starting activity")

	if request == nil {
		err := errors.New("received empty SetTenantIdentityConfiguration request")
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}
	if request.GetOrganizationId() == "" {
		err := errors.New("received SetTenantIdentityConfiguration request missing organization_id")
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}
	if request.GetConfig() == nil {
		err := errors.New("received SetTenantIdentityConfiguration request missing config")
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	carbideClient := m.CoreGrpcAtomicClient.GetClient()
	if carbideClient == nil {
		return nil, client.ErrCoreGrpcClientNotConnected
	}

	response, err := carbideClient.GrpcServiceClient().SetTenantIdentityConfiguration(ctx, request)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to set tenant identity configuration via Core gRPC API")
		return nil, swe.WrapErr(err)
	}

	logger.Info().Msg("Completed activity")
	return response, nil
}

// GetTenantIdentityConfigurationFromSite is an activity to get Tenant Identity Config using Core gRPC API
func (m *ManageTenantIdentity) GetTenantIdentityConfigurationFromSite(
	ctx context.Context,
	request *corev1.GetTenantIdentityConfigRequest,
) (*corev1.TenantIdentityConfigResponse, error) {
	logger := log.With().Str("Activity", "GetTenantIdentityConfigurationFromSite").Logger()
	logger.Info().Msg("Starting activity")

	if request == nil {
		err := errors.New("received empty GetTenantIdentityConfiguration request")
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}
	if request.GetOrganizationId() == "" {
		err := errors.New("received GetTenantIdentityConfiguration request missing organization_id")
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	carbideClient := m.CoreGrpcAtomicClient.GetClient()
	if carbideClient == nil {
		return nil, client.ErrCoreGrpcClientNotConnected
	}

	response, err := carbideClient.GrpcServiceClient().GetTenantIdentityConfiguration(ctx, request)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to get tenant identity configuration via Core gRPC API")
		return nil, swe.WrapErr(err)
	}

	logger.Info().Msg("Completed activity")
	return response, nil
}

// DeleteTenantIdentityConfigurationOnSite is an activity to delete Tenant Identity Config using Core gRPC API
func (m *ManageTenantIdentity) DeleteTenantIdentityConfigurationOnSite(
	ctx context.Context,
	request *corev1.GetTenantIdentityConfigRequest,
) (*emptypb.Empty, error) {
	logger := log.With().Str("Activity", "DeleteTenantIdentityConfigurationOnSite").Logger()
	logger.Info().Msg("Starting activity")

	if request == nil {
		err := errors.New("received empty DeleteTenantIdentityConfiguration request")
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}
	if request.GetOrganizationId() == "" {
		err := errors.New("received DeleteTenantIdentityConfiguration request missing organization_id")
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	carbideClient := m.CoreGrpcAtomicClient.GetClient()
	if carbideClient == nil {
		return nil, client.ErrCoreGrpcClientNotConnected
	}

	response, err := carbideClient.GrpcServiceClient().DeleteTenantIdentityConfiguration(ctx, request)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to delete tenant identity configuration via Core gRPC API")
		return nil, swe.WrapErr(err)
	}

	logger.Info().Msg("Completed activity")
	return response, nil
}

// CreateOrUpdateTenantIdentityTokenDelegationOnSite is an activity to create or update Token Delegation using Core gRPC API
func (m *ManageTenantIdentity) CreateOrUpdateTenantIdentityTokenDelegationOnSite(
	ctx context.Context,
	request *corev1.TokenDelegationRequest,
) (*corev1.TokenDelegationResponse, error) {
	logger := log.With().Str("Activity", "CreateOrUpdateTenantIdentityTokenDelegationOnSite").Logger()
	logger.Info().Msg("Starting activity")

	if request == nil {
		err := errors.New("received empty SetTenantIdentityTokenDelegation request")
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}
	if request.GetOrganizationId() == "" {
		err := errors.New("received SetTenantIdentityTokenDelegation request missing organization_id")
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}
	if request.GetConfig() == nil {
		err := errors.New("received SetTenantIdentityTokenDelegation request missing config")
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	carbideClient := m.CoreGrpcAtomicClient.GetClient()
	if carbideClient == nil {
		return nil, client.ErrCoreGrpcClientNotConnected
	}

	response, err := carbideClient.GrpcServiceClient().SetTokenDelegation(ctx, request)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to set token delegation via Core gRPC API")
		return nil, swe.WrapErr(err)
	}

	logger.Info().Msg("Completed activity")
	return response, nil
}

// GetTenantIdentityTokenDelegationFromSite is an activity to get Token Delegation using Core gRPC API
func (m *ManageTenantIdentity) GetTenantIdentityTokenDelegationFromSite(
	ctx context.Context,
	request *corev1.GetTokenDelegationRequest,
) (*corev1.TokenDelegationResponse, error) {
	logger := log.With().Str("Activity", "GetTenantIdentityTokenDelegationFromSite").Logger()
	logger.Info().Msg("Starting activity")

	if request == nil {
		err := errors.New("received empty GetTenantIdentityTokenDelegation request")
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}
	if request.GetOrganizationId() == "" {
		err := errors.New("received GetTenantIdentityTokenDelegation request missing organization_id")
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	carbideClient := m.CoreGrpcAtomicClient.GetClient()
	if carbideClient == nil {
		return nil, client.ErrCoreGrpcClientNotConnected
	}

	response, err := carbideClient.GrpcServiceClient().GetTokenDelegation(ctx, request)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to get token delegation via Core gRPC API")
		return nil, swe.WrapErr(err)
	}

	logger.Info().Msg("Completed activity")
	return response, nil
}

// DeleteTenantIdentityTokenDelegationOnSite is an activity to delete Token Delegation using Core gRPC API
func (m *ManageTenantIdentity) DeleteTenantIdentityTokenDelegationOnSite(
	ctx context.Context,
	request *corev1.GetTokenDelegationRequest,
) (*emptypb.Empty, error) {
	logger := log.With().Str("Activity", "DeleteTenantIdentityTokenDelegationOnSite").Logger()
	logger.Info().Msg("Starting activity")

	if request == nil {
		err := errors.New("received empty DeleteTenantIdentityTokenDelegation request")
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}
	if request.GetOrganizationId() == "" {
		err := errors.New("received DeleteTenantIdentityTokenDelegation request missing organization_id")
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	carbideClient := m.CoreGrpcAtomicClient.GetClient()
	if carbideClient == nil {
		return nil, client.ErrCoreGrpcClientNotConnected
	}

	response, err := carbideClient.GrpcServiceClient().DeleteTokenDelegation(ctx, request)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to delete token delegation via Core gRPC API")
		return nil, swe.WrapErr(err)
	}

	logger.Info().Msg("Completed activity")
	return response, nil
}

// GetJWKSFromSite is an activity to get JWKS using Core gRPC API
func (m *ManageTenantIdentity) GetJWKSFromSite(
	ctx context.Context,
	request *corev1.JwksRequest,
) (*corev1.Jwks, error) {
	logger := log.With().Str("Activity", "GetJWKSFromSite").Logger()
	logger.Info().Msg("Starting activity")

	if request == nil {
		err := errors.New("received empty GetJWKS request")
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}
	if request.GetOrganizationId() == "" {
		err := errors.New("received GetJWKS request missing organization_id")
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	carbideClient := m.CoreGrpcAtomicClient.GetClient()
	if carbideClient == nil {
		return nil, client.ErrCoreGrpcClientNotConnected
	}

	response, err := carbideClient.GrpcServiceClient().GetJWKS(ctx, request)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to get JWKS via Core gRPC API")
		return nil, swe.WrapErr(err)
	}

	logger.Info().Msg("Completed activity")
	return response, nil
}

// GetOpenIDConfigurationFromSite is an activity to get OpenID Configuration using Core gRPC API
func (m *ManageTenantIdentity) GetOpenIDConfigurationFromSite(
	ctx context.Context,
	request *corev1.OpenIdConfigRequest,
) (*corev1.OpenIdConfiguration, error) {
	logger := log.With().Str("Activity", "GetOpenIDConfigurationFromSite").Logger()
	logger.Info().Msg("Starting activity")

	if request == nil {
		err := errors.New("received empty GetOpenIDConfiguration request")
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}
	if request.GetOrganizationId() == "" {
		err := errors.New("received GetOpenIDConfiguration request missing organization_id")
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	carbideClient := m.CoreGrpcAtomicClient.GetClient()
	if carbideClient == nil {
		return nil, client.ErrCoreGrpcClientNotConnected
	}

	response, err := carbideClient.GrpcServiceClient().GetOpenIDConfiguration(ctx, request)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to get OpenID configuration via Core gRPC API")
		return nil, swe.WrapErr(err)
	}

	logger.Info().Msg("Completed activity")
	return response, nil
}
