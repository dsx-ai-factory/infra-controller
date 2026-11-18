// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"fmt"
	"net/http"
	"time"

	"github.com/labstack/echo/v4"
	k8serr "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/site-manager/internal/certmanager"
	crdclientv1 "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/client/clientset/versioned/typed/crds/v1"
	crdsv1 "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/crds/v1"
	"github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/types"
)

// clientCertificateTTLHours is the validity of a Site's client certificate,
// `90 days`.
const clientCertificateTTLHours = 90 * 24

// CreateSiteCredsHandler exchanges a Site's OTP for its client credentials.
type CreateSiteCredsHandler struct {
	sites       crdclientv1.SiteInterface
	certManager *certmanager.Client
}

// NewCreateSiteCredsHandler returns a handler that checks OTPs against sites
// and gets certificates from certManager.
func NewCreateSiteCredsHandler(sites crdclientv1.SiteInterface, certManager *certmanager.Client) CreateSiteCredsHandler {
	return CreateSiteCredsHandler{sites: sites, certManager: certManager}
}

// Handle returns a client key, certificate and CA certificate for a Site
// waiting for its handshake, then marks the handshake complete. Every failure
// after decoding returns `500`, as Site Agent expects.
func (csch CreateSiteCredsHandler) Handle(c echo.Context) error {
	ctx := c.Request().Context()
	logger := newLogger(c, "SiteCreds", "Create")

	req := &types.SiteCredsRequest{}
	if err := json.NewDecoder(c.Request().Body).Decode(req); err != nil {
		logger.Warn().Err(err).Msg("failed to decode the Site credentials request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	objName := nameFromUUID(req.SiteUUID)
	// Logs show only the start of the OTP that arrived. It stays usable until
	// a handshake consumes it.
	logger = logger.With().
		Str("Site", objName).
		Str("OTP", cutil.RedactSecret(req.OTP, cutil.SecretLogPrefixLen)).
		Logger()

	siteObj, err := csch.sites.Get(ctx, objName, metav1.GetOptions{})
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Site")
		message := "Unable to retrieve Site, check logs"
		if k8serr.IsNotFound(err) {
			message = fmt.Sprintf("Site %s not found", req.SiteUUID)
		}
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, message, nil)
	}

	if siteObj.Status.BootstrapState != crdsv1.SiteAwaitHandshake {
		logger.Info().Msg("credentials requested with a used OTP")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "OTP already used", nil)
	}

	if req.OTP != siteObj.Status.OTP.Passcode {
		logger.Info().Msg("credentials requested with a wrong OTP")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Bad OTP", nil)
	}

	expiry, err := parseExpiry(&siteObj.Status.OTP)
	if err != nil {
		logger.Error().Err(err).Str("OTP Expiry", siteObj.Status.OTP.Timestamp).Msg("failed to parse the OTP expiry")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to parse OTP expiry, check logs", nil)
	}

	if time.Now().After(*expiry) {
		logger.Info().Msg("credentials requested with an expired OTP")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "OTP expired", nil)
	}

	logger.Info().Msg("getting credentials for Site")
	certificate, err := csch.certManager.IssueCertificate(ctx, "client", req.SiteUUID, clientCertificateTTLHours)
	if err != nil {
		logger.Error().Err(err).Msg("failed to issue a client certificate")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Error getting creds, check logs", nil)
	}

	ca, err := csch.certManager.GetCA(ctx)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get the CA certificate")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Error getting CA, check logs", nil)
	}

	siteObj.Status.BootstrapState = crdsv1.SiteHandshakeComplete
	if _, err := csch.sites.UpdateStatus(ctx, siteObj, metav1.UpdateOptions{}); err != nil {
		logger.Error().Err(err).Msg("failed to update Site status")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to update Site status, check logs", nil)
	}

	return c.JSON(http.StatusOK, &types.SiteCredsResponse{
		Key:           certificate.Key,
		Certificate:   certificate.Certificate,
		CACertificate: ca,
	})
}
