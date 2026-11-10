// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package handler holds Site Manager's route handlers.
package handler

import (
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"net/http"
	"time"

	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	k8serr "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	crdclientv1 "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/client/clientset/versioned/typed/crds/v1"
	crdsv1 "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/crds/v1"
	"github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/types"
)

const (
	// otpLength is the number of random bytes in an OTP, before base64.
	otpLength = 20

	// errorUUIDRequired is returned when the path doesn't include a Site UUID.
	errorUUIDRequired = "Site UUID is required"
)

// CreateSiteHandler creates the Site resource and its first OTP.
type CreateSiteHandler struct {
	sites       crdclientv1.SiteInterface
	otpDuration time.Duration
}

// NewCreateSiteHandler returns a handler that stores Sites in sites. Their
// OTPs expire after otpDuration.
func NewCreateSiteHandler(sites crdclientv1.SiteInterface, otpDuration time.Duration) CreateSiteHandler {
	return CreateSiteHandler{sites: sites, otpDuration: otpDuration}
}

// Handle creates the Site resource and sets its status to wait for the
// handshake. It returns `400` for an invalid body or an existing Site.
func (csh CreateSiteHandler) Handle(c echo.Context) error {
	ctx := c.Request().Context()
	logger := newLogger(c, "Site", "Create")

	// The REST API doesn't send a Content-Type, so the body is decoded here
	// rather than with c.Bind.
	req := &types.SiteCreateRequest{}
	if err := json.NewDecoder(c.Request().Body).Decode(req); err != nil {
		logger.Warn().Err(err).Msg("failed to decode the Site create request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	objName := nameFromUUID(req.SiteUUID)
	logger = logger.With().Str("Site", objName).Logger()

	otp, err := generateOTP(csh.otpDuration)
	if err != nil {
		logger.Error().Err(err).Msg("failed to generate an OTP")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate OTP for Site", nil)
	}

	siteCRD := &crdsv1.Site{
		ObjectMeta: metav1.ObjectMeta{Name: objName},
		Spec: crdsv1.SiteSpec{
			UUID:     req.SiteUUID,
			SiteName: req.Name,
			Provider: req.Provider,
			FCOrg:    req.FCOrg,
		},
	}

	if _, err := csh.sites.Create(ctx, siteCRD, metav1.CreateOptions{}); err != nil {
		logger.Error().Err(err).Msg("failed to create Site")
		if k8serr.IsAlreadyExists(err) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site already exists", nil)
		}
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create Site", nil)
	}

	siteObj, err := csh.sites.Get(ctx, objName, metav1.GetOptions{})
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve created Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve created Site", nil)
	}
	siteObj.Status = crdsv1.SiteStatus{
		OTP:            *otp,
		BootstrapState: crdsv1.SiteAwaitHandshake,
	}
	if _, err := csh.sites.UpdateStatus(ctx, siteObj, metav1.UpdateOptions{}); err != nil {
		logger.Error().Err(err).Msg("failed to update Site status")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to update Site status", nil)
	}

	logger.Info().Str("OTP Expiry", otp.Timestamp).Msg("created Site")
	return c.NoContent(http.StatusOK)
}

// GetSiteHandler returns a Site with its OTP.
type GetSiteHandler struct {
	sites crdclientv1.SiteInterface
}

// NewGetSiteHandler returns a handler that reads Sites from sites.
func NewGetSiteHandler(sites crdclientv1.SiteInterface) GetSiteHandler {
	return GetSiteHandler{sites: sites}
}

// Handle returns the Site as a types.SiteGetResponse. It returns `404` for an
// unknown Site.
func (gsh GetSiteHandler) Handle(c echo.Context) error {
	logger := newLogger(c, "Site", "Get")

	uuid := c.Param("uuid")
	if uuid == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errorUUIDRequired, nil)
	}
	objName := nameFromUUID(uuid)
	logger = logger.With().Str("Site", objName).Logger()
	siteObj, err := gsh.sites.Get(c.Request().Context(), objName, metav1.GetOptions{})
	if err != nil {
		return siteError(c, logger, err, "failed to retrieve Site", "Failed to retrieve Site")
	}

	resp := &types.SiteGetResponse{
		SiteUUID:           uuid,
		Name:               siteObj.Spec.SiteName,
		Provider:           siteObj.Spec.Provider,
		FCOrg:              siteObj.Spec.FCOrg,
		BootstrapState:     siteObj.Status.BootstrapState,
		ControlPlaneStatus: siteObj.Status.ControlPlaneStatus,
		OTP:                siteObj.Status.OTP.Passcode,
	}
	// A Site without a valid OTP expiry is returned without one.
	if exp, err := parseExpiry(&siteObj.Status.OTP); err == nil {
		resp.OTPExpiry = exp.String()
	}
	return c.JSON(http.StatusOK, resp)
}

// DeleteSiteHandler deletes a Site.
type DeleteSiteHandler struct {
	sites crdclientv1.SiteInterface
}

// NewDeleteSiteHandler returns a handler that deletes Sites from sites.
func NewDeleteSiteHandler(sites crdclientv1.SiteInterface) DeleteSiteHandler {
	return DeleteSiteHandler{sites: sites}
}

// Handle deletes the Site resource. It returns `404` for an unknown Site,
// which the REST API treats as already deleted.
func (dsh DeleteSiteHandler) Handle(c echo.Context) error {
	ctx := c.Request().Context()
	logger := newLogger(c, "Site", "Delete")

	uuid := c.Param("uuid")
	if uuid == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errorUUIDRequired, nil)
	}
	objName := nameFromUUID(uuid)
	logger = logger.With().Str("Site", objName).Logger()
	if err := dsh.sites.Delete(ctx, objName, metav1.DeleteOptions{}); err != nil {
		return siteError(c, logger, err, "failed to delete Site", "Failed to delete Site")
	}

	logger.Info().Msg("deleted Site")
	return c.NoContent(http.StatusOK)
}

// RegisterSiteHandler marks a Site's registration as complete.
type RegisterSiteHandler struct {
	sites crdclientv1.SiteInterface
}

// NewRegisterSiteHandler returns a handler that updates Sites in sites.
func NewRegisterSiteHandler(sites crdclientv1.SiteInterface) RegisterSiteHandler {
	return RegisterSiteHandler{sites: sites}
}

// Handle sets the Site's bootstrap state to registration complete. It returns
// `404` for an unknown Site.
func (rsh RegisterSiteHandler) Handle(c echo.Context) error {
	ctx := c.Request().Context()
	logger := newLogger(c, "Site", "Register")

	uuid := c.Param("uuid")
	if uuid == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errorUUIDRequired, nil)
	}
	objName := nameFromUUID(uuid)
	logger = logger.With().Str("Site", objName).Logger()
	siteObj, err := rsh.sites.Get(ctx, objName, metav1.GetOptions{})
	if err != nil {
		return siteError(c, logger, err, "failed to retrieve Site", "Failed to retrieve Site")
	}

	siteObj.Status.BootstrapState = crdsv1.SiteRegistrationComplete
	if _, err := rsh.sites.UpdateStatus(ctx, siteObj, metav1.UpdateOptions{}); err != nil {
		return siteError(c, logger, err, "failed to update Site status", "Failed to update Site status")
	}

	logger.Info().Msg("Site registration complete")
	return c.NoContent(http.StatusOK)
}

// RollSiteHandler replaces a Site's OTP.
type RollSiteHandler struct {
	sites       crdclientv1.SiteInterface
	otpDuration time.Duration
}

// NewRollSiteHandler returns a handler that updates Sites in sites. New OTPs
// expire after otpDuration.
func NewRollSiteHandler(sites crdclientv1.SiteInterface, otpDuration time.Duration) RollSiteHandler {
	return RollSiteHandler{sites: sites, otpDuration: otpDuration}
}

// Handle gives the Site a new OTP and sets it to wait for the handshake again.
// It returns `404` for an unknown Site.
func (rsh RollSiteHandler) Handle(c echo.Context) error {
	ctx := c.Request().Context()
	logger := newLogger(c, "Site", "Roll")

	uuid := c.Param("uuid")
	if uuid == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errorUUIDRequired, nil)
	}
	objName := nameFromUUID(uuid)
	logger = logger.With().Str("Site", objName).Logger()
	siteObj, err := rsh.sites.Get(ctx, objName, metav1.GetOptions{})
	if err != nil {
		return siteError(c, logger, err, "failed to retrieve Site", "Failed to retrieve Site")
	}

	otp, err := generateOTP(rsh.otpDuration)
	if err != nil {
		logger.Error().Err(err).Msg("failed to generate an OTP")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate OTP for Site", nil)
	}

	siteObj.Status.OTP = *otp
	siteObj.Status.BootstrapState = crdsv1.SiteAwaitHandshake
	if _, err := rsh.sites.UpdateStatus(ctx, siteObj, metav1.UpdateOptions{}); err != nil {
		return siteError(c, logger, err, "failed to update Site status", "Failed to update Site status")
	}

	logger.Info().Str("OTP Expiry", otp.Timestamp).Msg("rolled Site")
	return c.NoContent(http.StatusOK)
}

// newLogger returns the logger of one handler, with the request's trace IDs.
// Like the REST API's handlers, it logs that the handler started.
func newLogger(c echo.Context, model, handler string) zerolog.Logger {
	logger := log.With().Str("Model", model).Str("Handler", handler).Logger()
	logger = cotel.LoggerWithTrace(c.Request().Context(), logger)
	logger.Info().Msg("started API handler")
	return logger
}

// siteError logs err with logMessage. It sends `404` when the Site doesn't
// exist, or `500` with message otherwise.
func siteError(c echo.Context, logger zerolog.Logger, err error, logMessage, message string) error {
	logger.Error().Err(err).Msg(logMessage)
	if k8serr.IsNotFound(err) {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Site not found", nil)
	}
	return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, message, nil)
}

// generateOTP returns a random passcode that expires after duration.
func generateOTP(duration time.Duration) (*crdsv1.OTPInfo, error) {
	b := make([]byte, otpLength)
	if _, err := rand.Read(b); err != nil {
		return nil, err
	}

	expiryTS, err := time.Now().Add(duration).MarshalText()
	if err != nil {
		return nil, err
	}
	return &crdsv1.OTPInfo{
		Passcode:  base64.URLEncoding.EncodeToString(b),
		Timestamp: string(expiryTS),
	}, nil
}

// parseExpiry returns the expiry time of o.
func parseExpiry(o *crdsv1.OTPInfo) (*time.Time, error) {
	if o.Timestamp == "" {
		return nil, fmt.Errorf("empty timestamp")
	}
	t := &time.Time{}
	if err := t.UnmarshalText([]byte(o.Timestamp)); err != nil {
		return nil, err
	}
	return t, nil
}

// nameFromUUID returns the name of the Site resource for a Site UUID.
func nameFromUUID(uuid string) string {
	return fmt.Sprintf("site-%s", uuid)
}
