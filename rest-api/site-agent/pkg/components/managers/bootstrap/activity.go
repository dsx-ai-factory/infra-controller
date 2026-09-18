// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package bootstrap

import (
	"context"
	"crypto/x509"
	"encoding/base64"
	"encoding/pem"
	"errors"

	cutils "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/temporal"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	coreV1Types "k8s.io/client-go/kubernetes/typed/core/v1"
)

// OTPHandler handles OTP related activities
type OTPHandler struct {
	SecretInterface coreV1Types.SecretInterface
}

// ReceiveAndSaveOTP receives a new OTP and updates the bootstrap info
func (o *OTPHandler) ReceiveAndSaveOTP(ctx context.Context, base64EncodedEncryptedOtp string) error {
	logger := log.With().Str("Activity", "ReceiveAndSaveOTP").Logger()
	logger.Info().Msg("Starting activity to receive and save OTP")

	if base64EncodedEncryptedOtp == "" {
		err := errors.New("received empty OTP")
		return temporal.NewNonRetryableApplicationError(err.Error(), "ErrEmptyOTP", err)
	}

	// Base64 decode the OTP
	encryptedOtpBytes, err := base64.StdEncoding.DecodeString(base64EncodedEncryptedOtp)
	if err != nil {
		logger.Error().Err(err).Str("OTP", cutils.RedactSecret(base64EncodedEncryptedOtp, cutils.SecretLogPrefixLen)).
			Msg("Failed to decode Base64 OTP")
		return temporal.NewNonRetryableApplicationError(err.Error(), "ErrBase64DecodeOTP", err)
	}

	if o.SecretInterface == nil {
		err = errors.New("secretInterface is not set")
		return temporal.NewNonRetryableApplicationError(err.Error(), "ErrNilSecretInterface", err)
	}

	// Get the bootstrap-info secret, which contains the OTP to act on
	bootstrapInfoSecret, err := o.SecretInterface.Get(ctx, "bootstrap-info", metav1.GetOptions{})
	if err != nil {
		logger.Error().Err(err).Msg("Failed to read bootstrap-info secret")
		return err
	}

	// Check if Data is nil and raise an error if true
	if bootstrapInfoSecret.Data == nil {
		err := errors.New("bootstrap-info secret data is nil")
		logger.Error().Err(err).Msg(err.Error())
		return temporal.NewNonRetryableApplicationError(err.Error(), "ErrNilSecretData", err)
	}

	// Decrypt the new OTP using the siteID
	decryptedOtp := cutils.DecryptData(encryptedOtpBytes, ManagerAccess.Conf.EB.Temporal.ClusterID)

	// Update the OTP in the bootstrap-info secret without base64 encoding
	bootstrapInfoSecret.Data["otp"] = []byte(decryptedOtp)

	_, err = o.SecretInterface.Update(ctx, bootstrapInfoSecret, metav1.UpdateOptions{})
	if err != nil {
		logger.Error().Err(err).Msg("Failed to update bootstrap-info secret")
		return err
	}

	logger.Info().Msg("Successfully updated OTP in bootstrap-info secret")

	// Proceed to download and store credentials, passing the decrypted OTP
	err = ManagerAccess.API.Bootstrap.DownloadAndStoreCreds(decryptedOtp)
	if err != nil {
		logger.Error().Err(err).Msg("Failed to download and store credentials")
		return err
	}

	// DownloadAndStoreCreds reads the new OTP from the bootstrap secret and writes the certs to Temporal cert secret
	// We cannot read the cert directly from file since K8s secret updates are propagated to the pod file system over time

	// Read the Temporal secret to get the CA certificate
	temporalSecret, err := o.SecretInterface.Get(ctx, ManagerAccess.Conf.EB.TemporalSecret, metav1.GetOptions{})
	if err != nil {
		logger.Error().Err(err).Msg("Failed to read Temporal cert secret after cert rotation")
		return err
	}

	// Get the CA certificate from the secret
	if temporalSecret.Data == nil {
		logger.Error().Err(err).Msg("Failed to read Temporal cert secret after cert rotation")
		return errors.New("failed to read Temporal cert secret after cert rotation")
	}

	// Secrets read using Go SDK is returned as original byte value, not base64 encoded
	certData, ok := temporalSecret.Data["certificate"]
	if !ok {
		logger.Error().Err(err).Msg("Failed to read certificate from Temporal cert secret")
		return errors.New("failed to read certificate from Temporal cert secret")
	}

	// Decode the PEM block containing the certificate
	block, _ := pem.Decode(certData)
	if block == nil {
		logger.Error().Err(err).Msg("failed to parse certificate PEM")
		return errors.New("failed to parse certificate PEM")
	}

	// Parse the certificate
	parsedCert, err := x509.ParseCertificate(block.Bytes)
	if err != nil {
		logger.Error().Err(err).Msg("failed to parse certificate")
		return err
	}
	certExpiry := parsedCert.NotAfter

	logger.Info().Msgf("Certificate expiration date: %v", certExpiry)

	siteID := ManagerAccess.Data.EB.Managers.Bootstrap.Config.UUID
	workflowOptions := client.StartWorkflowOptions{
		ID:        "update-agent-cert-expiry-" + siteID,
		TaskQueue: ManagerAccess.Conf.EB.Temporal.TemporalPublishQueue,
	}

	tcPublish := ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher
	we, werr := tcPublish.ExecuteWorkflow(ctx, workflowOptions, "UpdateAgentCertExpiry", siteID, certExpiry)
	if werr != nil {
		logger.Error().Err(werr).Msg("Failed to start UpdateAgentCertExpiry")
		return werr
	} else {
		logger.Info().Msgf("Started UpdateAgentCertExpiry with WorkflowID: %s", we.GetID())
	}

	logger.Info().Msg("Successfully completed the activity")
	return nil
}

// NewOTPHandler returns a new OTPHandler activity
func NewOTPHandler(secretInterface coreV1Types.SecretInterface) OTPHandler {
	return OTPHandler{
		SecretInterface: secretInterface,
	}
}
