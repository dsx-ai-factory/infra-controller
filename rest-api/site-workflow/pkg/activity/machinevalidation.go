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
)

// ManageMachineValidation is an activity wrapper for Machine Validation management
type ManageMachineValidation struct {
	coreGrpcAtomicClient *client.CoreGrpcAtomicClient
}

// NewManageMachineValidation returns a new ManageMachineValidation client
func NewManageMachineValidation(coreGrpcClient *client.CoreGrpcAtomicClient) ManageMachineValidation {
	return ManageMachineValidation{
		coreGrpcAtomicClient: coreGrpcClient,
	}
}

func (mmv *ManageMachineValidation) EnableDisableMachineValidationTestOnSite(ctx context.Context, request *corev1.MachineValidationTestEnableDisableTestRequest) error {
	logger := log.With().Str("Activity", "EnableDisableMachineValidationTestOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty enable/disable machine validation test request")
	} else if request.TestId == "" {
		err = errors.New("received enable/disable machine validation test request missing TestId")
	} else if request.Version == "" {
		err = errors.New("received enable/disable machine validation test request missing Version")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mmv.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return client.ErrCoreGrpcClientNotConnected
	}

	grpcServiceClient := grpcClient.GrpcServiceClient()

	_, err = grpcServiceClient.MachineValidationTestEnableDisableTest(ctx, request)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to enable/disable machine validation test using Core gRPC API")
		return swe.WrapErr(err)
	}

	logger.Info().Msg("Completed activity")

	return nil
}

func (mmv *ManageMachineValidation) PersistValidationResultOnSite(ctx context.Context, request *corev1.MachineValidationResultPostRequest) error {
	logger := log.With().Str("Activity", "PersistValidationResultOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty persist validation results request")
	} else if request.Result == nil {
		err = errors.New("received persist validation results request missing Result")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mmv.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return client.ErrCoreGrpcClientNotConnected
	}

	grpcServiceClient := grpcClient.GrpcServiceClient()

	_, err = grpcServiceClient.PersistValidationResult(ctx, request)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to persist validation results using Core gRPC API")
		return swe.WrapErr(err)
	}

	logger.Info().Msg("Completed activity")

	return nil
}

func (mmv *ManageMachineValidation) GetMachineValidationResultsFromSite(ctx context.Context, request *corev1.MachineValidationGetRequest) (*corev1.MachineValidationResultList, error) {
	logger := log.With().Str("Activity", "GetMachineValidationResultsFromSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty get machine validation results request")
	}

	if err != nil {
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC endpoint
	grpcClient := mmv.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return nil, client.ErrCoreGrpcClientNotConnected
	}

	grpcServiceClient := grpcClient.GrpcServiceClient()

	result, err := grpcServiceClient.GetMachineValidationResults(ctx, request)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to get machine validation results using Core gRPC API")
		return nil, swe.WrapErr(err)
	}

	logger.Info().Msg("Completed activity")

	return result, nil
}

func (mmv *ManageMachineValidation) GetMachineValidationRunsFromSite(ctx context.Context, request *corev1.MachineValidationRunListGetRequest) (*corev1.MachineValidationRunList, error) {
	logger := log.With().Str("Activity", "GetMachineValidationRunsFromSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty get machine validation runs request")
	} else if request.MachineId == nil {
		err = errors.New("received get machine validation runs request missing MachineId")
	}

	if err != nil {
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC endpoint
	grpcClient := mmv.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return nil, client.ErrCoreGrpcClientNotConnected
	}

	grpcServiceClient := grpcClient.GrpcServiceClient()

	result, err := grpcServiceClient.GetMachineValidationRuns(ctx, request)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to get machine validation runs using Core gRPC API")
		return nil, err
	}

	logger.Info().Msg("Completed activity")

	return result, nil
}

func (mmv *ManageMachineValidation) GetMachineValidationTestsFromSite(ctx context.Context, request *corev1.MachineValidationTestsGetRequest) (*corev1.MachineValidationTestsGetResponse, error) {
	logger := log.With().Str("Activity", "GetMachineValidationTestsFromSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty get machine validation tests request")
	}

	if err != nil {
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC endpoint
	grpcClient := mmv.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return nil, client.ErrCoreGrpcClientNotConnected
	}

	grpcServiceClient := grpcClient.GrpcServiceClient()

	result, err := grpcServiceClient.GetMachineValidationTests(ctx, request)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to get machine validation tests using Core gRPC API")
		return nil, swe.WrapErr(err)
	}

	logger.Info().Msg("Completed activity")

	return result, nil
}

func (mmv *ManageMachineValidation) AddMachineValidationTestOnSite(ctx context.Context, request *corev1.MachineValidationTestAddRequest) (*corev1.MachineValidationTestAddUpdateResponse, error) {
	logger := log.With().Str("Activity", "AddMachineValidationTestOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty add machine validation test request")
	} else if request.Name == "" {
		err = errors.New("received add machine validation test request missing Name")
	} else if request.Command == "" {
		err = errors.New("received add machine validation test request missing Command")
	} else if request.Args == "" {
		err = errors.New("received add machine validation test request missing Args")
	}

	if err != nil {
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC endpoint
	grpcClient := mmv.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return nil, client.ErrCoreGrpcClientNotConnected
	}

	grpcServiceClient := grpcClient.GrpcServiceClient()

	response, err := grpcServiceClient.AddMachineValidationTest(ctx, request)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to add machine validation test using Core gRPC API")
		return nil, swe.WrapErr(err)
	}

	logger.Info().Msg("Completed activity")

	return response, nil
}

func (mmv *ManageMachineValidation) UpdateMachineValidationTestOnSite(ctx context.Context, request *corev1.MachineValidationTestUpdateRequest) error {
	logger := log.With().Str("Activity", "UpdateMachineValidationTestOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty update machine validation test request")
	} else if request.TestId == "" {
		err = errors.New("received update machine validation test request missing TestId")
	} else if request.Version == "" {
		err = errors.New("received update machine validation test request missing Version")
	} else if request.Payload == nil {
		err = errors.New("received update machine validation test request missing Payload")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC API endpoint
	grpcClient := mmv.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return client.ErrCoreGrpcClientNotConnected
	}

	grpcServiceClient := grpcClient.GrpcServiceClient()

	_, err = grpcServiceClient.UpdateMachineValidationTest(ctx, request)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to update machine validation test using Core gRPC API")
		return swe.WrapErr(err)
	}

	logger.Info().Msg("Completed activity")

	return nil
}

func (mmv *ManageMachineValidation) GetMachineValidationExternalConfigsFromSite(ctx context.Context, request *corev1.GetMachineValidationExternalConfigsRequest) (*corev1.GetMachineValidationExternalConfigsResponse, error) {
	logger := log.With().Str("Activity", "GetMachineValidationExternalConfigsFromSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty get machine validation external configs request")
	}

	if err != nil {
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC endpoint
	grpcClient := mmv.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return nil, client.ErrCoreGrpcClientNotConnected
	}

	grpcServiceClient := grpcClient.GrpcServiceClient()

	result, err := grpcServiceClient.GetMachineValidationExternalConfigs(ctx, request)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to get machine validation external configs using Core gRPC API")
		return nil, swe.WrapErr(err)
	}

	logger.Info().Msg("Completed activity")

	return result, nil
}

func (mmv *ManageMachineValidation) AddUpdateMachineValidationExternalConfigOnSite(ctx context.Context, request *corev1.AddUpdateMachineValidationExternalConfigRequest) error {
	logger := log.With().Str("Activity", "AddUpdateMachineValidationExternalConfigOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty add/update machine validation external config request")
	} else if request.Name == "" {
		err = errors.New("received add/update machine validation external config request missing Name")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC endpoint
	grpcClient := mmv.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return client.ErrCoreGrpcClientNotConnected
	}

	grpcServiceClient := grpcClient.GrpcServiceClient()

	_, err = grpcServiceClient.AddUpdateMachineValidationExternalConfig(ctx, request)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to add/update machine validation external config using Core gRPC API")
		return swe.WrapErr(err)
	}

	logger.Info().Msg("Completed activity")

	return nil
}

func (mmv *ManageMachineValidation) RemoveMachineValidationExternalConfigOnSite(ctx context.Context, request *corev1.RemoveMachineValidationExternalConfigRequest) error {
	logger := log.With().Str("Activity", "RemoveMachineValidationExternalConfigOnSite").Logger()

	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if request == nil {
		err = errors.New("received empty remove machine validation external config request")
	} else if request.Name == "" {
		err = errors.New("received remove machine validation external config request missing Name")
	}

	if err != nil {
		return temporal.NewNonRetryableApplicationError(err.Error(), swe.ErrTypeInvalidRequest, err)
	}

	// Call Core gRPC endpoint
	grpcClient := mmv.coreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return client.ErrCoreGrpcClientNotConnected
	}

	grpcServiceClient := grpcClient.GrpcServiceClient()

	_, err = grpcServiceClient.RemoveMachineValidationExternalConfig(ctx, request)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to remove machine validation external config using Core gRPC API")
		return swe.WrapErr(err)
	}

	logger.Info().Msg("Completed activity")

	return nil
}
