// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"net/http"
	"strings"

	"github.com/rs/zerolog"
	tclient "go.temporal.io/sdk/client"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// Public attestation error messages shared by several endpoints.
const (
	errMsgAttestationProfileNotFound = "No Attestation Profile exists for this hardware class"
	errMsgAttestationJobNotFound     = "Could not find Attestation Job with specified ID"
	errMsgSiteMachineNotFound        = "Site could not find the Machine"
	errMsgSiteRejected               = "Site rejected the request"
	errMsgInvalidBodyPrefix          = "Invalid request body"
)

// Read-back guidance for mutations whose outcome is unknown.
const (
	readBackAttestationProfile = "Read the profile for this Site and hardware class before retrying."
	readBackAttestationJob     = "Read the job and check each device's cancellationTarget and cancellationRequestedAt before retrying."
	readBackMachineTrigger     = "List the Site's attestation jobs for this Machine before triggering again."
)

// attestationOperation describes one attestation endpoint's Core call so its
// failures can be reported without Temporal or backend diagnostics.
type attestationOperation struct {
	// fullMethod is the Core method the endpoint calls
	fullMethod string
	// failure describes the operation for generic failures, such as
	// "Failed to list Attestation Profiles"
	failure string
	// notFound is the 404 message; empty uses failure
	notFound string
	// conflict is the 409 message; empty uses failure
	conflict string
	// unavailable, when set, is the complete 503 message
	unavailable string
	// readBack, for a mutation, tells the operator how to learn whether a
	// request whose reply was lost was applied. Empty marks a read.
	readBack string
	// accepted is the 202 message of a fire-and-forget action
	accepted string
}

// Core operations behind the attestation endpoints. Keeping them together
// keeps each endpoint's public failure text and recovery guidance reviewable
// in one place.
var (
	opCreateAttestationProfile = attestationOperation{
		fullMethod: corev1.Forge_CreateAttestationProfile_FullMethodName,
		failure:    "Failed to create Attestation Profile",
		conflict:   "An Attestation Profile already exists for this hardware class",
		readBack:   readBackAttestationProfile,
	}
	opListAttestationProfiles = attestationOperation{
		fullMethod: corev1.Forge_ListAttestationProfiles_FullMethodName,
		failure:    "Failed to list Attestation Profiles",
	}
	opGetAttestationProfile = attestationOperation{
		fullMethod: corev1.Forge_GetAttestationProfile_FullMethodName,
		failure:    "Failed to retrieve Attestation Profile",
		notFound:   errMsgAttestationProfileNotFound,
	}
	opUpdateAttestationProfile = attestationOperation{
		fullMethod: corev1.Forge_UpdateAttestationProfile_FullMethodName,
		failure:    "Failed to update Attestation Profile",
		notFound:   errMsgAttestationProfileNotFound,
		readBack:   readBackAttestationProfile,
	}
	opDeleteAttestationProfile = attestationOperation{
		fullMethod: corev1.Forge_DeleteAttestationProfile_FullMethodName,
		failure:    "Failed to delete Attestation Profile",
		notFound:   errMsgAttestationProfileNotFound,
		readBack:   readBackAttestationProfile,
	}
	opGetAttestationCoverage = attestationOperation{
		fullMethod: corev1.Forge_GetAttestationCoverage_FullMethodName,
		failure:    "Failed to retrieve Attestation Coverage",
	}
	opListAttestationJobs = attestationOperation{
		fullMethod: corev1.Forge_ListAttestation_FullMethodName,
		failure:    "Failed to list Attestation Jobs",
	}
	opGetAttestationJob = attestationOperation{
		fullMethod: corev1.Forge_GetAttestation_FullMethodName,
		failure:    "Failed to retrieve Attestation Job",
		notFound:   errMsgAttestationJobNotFound,
	}
	opWaiveAttestationJob = attestationOperation{
		fullMethod: corev1.Forge_WaiveAttestation_FullMethodName,
		failure:    "Failed to waive Attestation Job",
		notFound:   errMsgAttestationJobNotFound,
		readBack:   readBackAttestationJob,
		accepted:   "Attestation Job waive request was accepted",
	}
	opFailAttestationJob = attestationOperation{
		fullMethod: corev1.Forge_FailAttestation_FullMethodName,
		failure:    "Failed to fail Attestation Job",
		notFound:   errMsgAttestationJobNotFound,
		readBack:   readBackAttestationJob,
		accepted:   "Attestation Job fail request was accepted",
	}
	opListMachineAttestationStatuses = attestationOperation{
		fullMethod: corev1.Forge_ListMachineAttestationStatuses_FullMethodName,
		failure:    "Failed to list Machine Attestation statuses",
	}
	opTriggerMachineAttestation = attestationOperation{
		fullMethod:  corev1.Forge_TriggerMachineAttestation_FullMethodName,
		failure:     "Failed to trigger Machine Attestation",
		notFound:    errMsgSiteMachineNotFound,
		unavailable: "SPDM may be disabled, or the Site reply may have been lost. " + readBackMachineTrigger,
		readBack:    readBackMachineTrigger,
	}
	opGetMachineAttestationStatus = attestationOperation{
		fullMethod: corev1.Forge_ListMachineAttestationStatuses_FullMethodName,
		failure:    "Failed to retrieve Machine Attestation status",
		notFound:   errMsgSiteMachineNotFound,
	}
)

// publicError replaces apiErr's message with curated text for its status,
// keeping the status and the original error as a private cause.
func (op attestationOperation) publicError(apiErr *cutil.APIError) *cutil.APIError {
	message := op.failure
	switch apiErr.Code {
	case http.StatusBadRequest:
		message = errMsgSiteRejected
		desc := coreStatusDescription(apiErr.Message)
		if desc != "" {
			message += ": " + desc
		}
	case http.StatusForbidden:
		message = "Site denied the request"
	case http.StatusNotFound:
		if op.notFound != "" {
			message = op.notFound
		}
	case http.StatusConflict:
		if op.conflict != "" {
			message = op.conflict
		}
	case http.StatusPreconditionFailed:
		message = "ifVersionMatch does not match the current version; read the profile again before retrying"
	case http.StatusNotImplemented:
		message = "Site does not support this attestation operation"
	case http.StatusServiceUnavailable:
		if op.unavailable != "" {
			message = op.unavailable
			break
		}
		message = op.withReadBack(message)
	case http.StatusTooManyRequests, http.StatusInternalServerError, http.StatusBadGateway, http.StatusGatewayTimeout:
		message = op.withReadBack(message)
	}
	return cutil.NewAPIError(apiErr.Code, message, nil).WithCause(apiErr)
}

// withReadBack warns, for a mutation, that a failed request may still have
// been applied, and says how to check.
func (op attestationOperation) withReadBack(message string) string {
	if op.readBack == "" {
		return message
	}
	return message + "; the change may have been applied. " + op.readBack
}

// coreStatusDescription extracts the description from a serialized Core gRPC
// status, dropping the gRPC and Temporal framing around it. It returns "" when
// the message has no description.
func coreStatusDescription(message string) string {
	_, desc, found := strings.Cut(message, "desc = ")
	if !found {
		return ""
	}
	cut := strings.LastIndex(desc, " (type: ")
	if cut >= 0 {
		desc = desc[:cut]
	}
	return strings.TrimSpace(desc)
}

// invalidReplyError logs and reports a Core reply that decoded but broke the
// endpoint's contract. For a mutation it may describe a committed change.
func (op attestationOperation) invalidReplyError(logger zerolog.Logger, cause error) *cutil.APIError {
	logger.Error().Err(cause).Str("method", op.fullMethod).Msg("invalid Core reply")
	return op.publicError(cutil.NewAPIError(http.StatusBadGateway, "", nil).WithCause(cause))
}

// call runs the operation's Core method with strict reply decoding.
func (op attestationOperation) call(ctx context.Context, logger zerolog.Logger, stc tclient.Client, siteID string, req, resp proto.Message) *cutil.APIError {
	apiErr := common.ExecuteCoreGRPCStrictResponse(ctx, stc, op.fullMethod, req, resp, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, op.failure)
		return op.publicError(apiErr)
	}
	return nil
}

// authorizeAttestationSite requires a Provider Admin of the org's provider and
// returns the client of the provider's Registered Site.
func authorizeAttestationSite(ctx context.Context, logger zerolog.Logger, dbSession *cdb.Session, scp *sc.ClientPool, org string, user *cdbm.User, siteID string) (tclient.Client, string, *cutil.APIError) {
	return common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx: ctx, Logger: logger, DBSession: dbSession, SCP: scp, Org: org, User: user, SiteID: siteID,
	})
}
