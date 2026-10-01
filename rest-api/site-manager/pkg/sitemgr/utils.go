// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sitemgr

import (
	"bytes"
	"context"
	"crypto/tls"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"

	"github.com/rs/zerolog"
	"go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	csmtypes "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/types"
)

const (
	siteManagerTimeout = 15 * time.Second
)

var (
	// ErrSiteNotFound is returned when a site is not found in cloud-site-manager.
	ErrSiteNotFound = errors.New("requested Site was not found in Site Manager")

	csmClient = &http.Client{
		Timeout: siteManagerTimeout,
		Transport: otelhttp.NewTransport(&http.Transport{
			// Disable certificate verification since CSM is an in-cluster server.
			TLSClientConfig: &tls.Config{InsecureSkipVerify: true},
		}, otelhttp.WithPropagators(cotel.Propagator())),
	}
)

// RollSite regenerates the bootstrap OTP for a site in cloud-site-manager.
// Instead of passing a site object, we now pass the necessary fields.
func RollSite(ctx context.Context, logger zerolog.Logger, siteID, siteName, url string) error {
	rollURL := fmt.Sprintf("%s/roll/%s", url, siteID)
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, rollURL, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to create request object for Site Manager")
		return err
	}
	err = httpDo(req, logger)
	if err == nil {
		logger.Info().Msg(fmt.Sprintf("Site Manager site %s/%s rolled", siteName, siteID))
	}
	return err
}

// GetSiteOTP retrieves the OTP and its expiration time for a given site UUID from cloud-site-manager.
func GetSiteOTP(ctx context.Context, logger zerolog.Logger, uuid, url string) (*string, *time.Time, error) {
	getURL := fmt.Sprintf("%s/%s", url, uuid)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, getURL, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to create request object for Site Manager")
		return nil, nil, err
	}
	resp, err := csmClient.Do(req)
	if err != nil || resp.StatusCode != http.StatusOK {
		logger.Error().Err(err).Msg("failed to retrieve Site from Site Manager")
		return nil, nil, fmt.Errorf("error getting Site from Site Manager")
	}
	defer resp.Body.Close()
	c, err := io.ReadAll(resp.Body)

	var siteResp csmtypes.SiteGetResponse
	err = json.Unmarshal(c, &siteResp)
	if err != nil {
		logger.Error().Err(err).Msg("failed to unmarshal Site Manager response")
		return nil, nil, err
	}

	exp, err := parseExpiryString(siteResp.OTPExpiry)
	if err != nil {
		logger.Error().Err(err).Msg("failed to parse Site Manager OTP expiration")
		return nil, nil, err
	}

	return &siteResp.OTP, exp, nil

}

// CreateSite creates a site in cloud-site-manager.
// It accepts the minimal fields needed to create a site.
func CreateSite(ctx context.Context, logger zerolog.Logger, siteUUID, name, provider, fcOrg, url string) error {
	scr := &csmtypes.SiteCreateRequest{
		SiteUUID: siteUUID,
		Name:     name,
		Provider: provider,
		FCOrg:    fcOrg,
	}

	b := new(bytes.Buffer)
	err := json.NewEncoder(b).Encode(scr)
	if err != nil {
		logger.Error().Err(err).Msg("failed to encode HTTP request into JSON")
		return err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, url, b)
	if err != nil {
		logger.Error().Err(err).Msg("failed to create request object for Site Manager")
		return err
	}
	err = httpDo(req, logger)
	if err == nil {
		logger.Info().Msg(fmt.Sprintf("Site Manager Site: %s/%s created", name, siteUUID))
	}
	return err
}

// DeleteSite deletes a site from cloud-site-manager using its UUID.
func DeleteSite(ctx context.Context, logger zerolog.Logger, uuid, url string) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodDelete, fmt.Sprintf("%s/%s", url, uuid), nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to create request object for Site Manager")
		return err
	}

	err = httpDo(req, logger)
	if err == nil {
		logger.Info().Msg(fmt.Sprintf("Site Manager Site: %s deleted", uuid))
	}
	return err
}

// httpDo executes the provided HTTP request and handles common error conditions.
func httpDo(req *http.Request, logger zerolog.Logger) error {
	resp, err := csmClient.Do(req)
	if err != nil {
		logger.Error().Err(err).Msg("failed to execute HTTP request against Site Manager")
		return err
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusOK {
		return nil
	}

	if resp.StatusCode == http.StatusNotFound {
		return ErrSiteNotFound
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		logger.Error().Err(err).Msg("failed to read Site Manager HTTP response body")
		return err
	}

	err = fmt.Errorf("HTTP request failed with status: %v", resp.StatusCode)
	logger.Error().Err(err).Msg(string(body))
	return err
}

// parseExpiry converts a timestamp string from the Site Manager into a time.Time object.
func parseExpiryString(ts string) (*time.Time, error) {
	if ts == "" {
		return nil, fmt.Errorf("empty timestamp supplied in argument")
	}

	layout := "2006-01-02 15:04:05.999999999 -0700 MST"
	// Some timestamps may include additional data (e.g., " m=..."), so we split it.
	t, err := time.Parse(layout, strings.Split(ts, " m=")[0])
	if err != nil {
		return nil, err
	}
	return &t, nil
}
