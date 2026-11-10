// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sites

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
	"github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/api"
	"github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/types"
)

const (
	siteManagerTimeout = 15 * time.Second
)

var (
	// ErrSiteNotFound is returned when a site is not found in Site Manager.
	ErrSiteNotFound = errors.New("requested Site was not found in Site Manager")

	// siteManagerClient sends every request to Site Manager.
	siteManagerClient = &SiteManagerClient{
		client: http.Client{
			Timeout: siteManagerTimeout,
			Transport: otelhttp.NewTransport(&http.Transport{
				// Disable certificate verification since Site Manager is an in-cluster server.
				TLSClientConfig: &tls.Config{InsecureSkipVerify: true},
			}, otelhttp.WithPropagators(cotel.Propagator())),
		},
	}

	// siteRoutes resolves the paths of Site Manager's Site routes.
	siteRoutes = api.NewSiteRouteResolver()
)

// SiteManagerClient sends requests to Site Manager.
type SiteManagerClient struct {
	client http.Client
}

// GetSiteURL returns the URL of the Site route called name, with params
// filled in. endpoint is the configured URL of the create Site route, such as
// https://nico-rest-site-manager:8100/v1/site. The other Site routes are
// resolved relative to it, so a path prefix in endpoint is kept.
func (smc *SiteManagerClient) GetSiteURL(endpoint, name string, params ...interface{}) string {
	createPath := siteRoutes.Reverse(api.CreateSiteRouteName)
	return endpoint + strings.TrimPrefix(siteRoutes.Reverse(name, params...), createPath)
}

// request sends req to Site Manager and returns the response body. `404`
// returns ErrSiteNotFound. Any other status except `200` is an error.
func (smc *SiteManagerClient) request(req *http.Request, logger zerolog.Logger) ([]byte, error) {
	resp, err := smc.client.Do(req)
	if err != nil {
		logger.Error().Err(err).Msg("failed to execute HTTP request against Site Manager")
		return nil, err
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusNotFound {
		return nil, ErrSiteNotFound
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		logger.Error().Err(err).Msg("failed to read Site Manager HTTP response body")
		return nil, err
	}

	if resp.StatusCode != http.StatusOK {
		err = fmt.Errorf("HTTP request failed with status: %v", resp.StatusCode)
		logger.Error().Err(err).Msg(string(body))
		return nil, err
	}

	return body, nil
}

// RollSite regenerates the bootstrap OTP for a site in Site Manager.
// Instead of passing a site object, we now pass the necessary fields.
func RollSite(ctx context.Context, logger zerolog.Logger, siteID, siteName, url string) error {
	rollURL := siteManagerClient.GetSiteURL(url, api.RollSiteRouteName, siteID)
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, rollURL, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to create request object for Site Manager")
		return err
	}
	_, err = siteManagerClient.request(req, logger)
	if err == nil {
		logger.Info().Msg(fmt.Sprintf("Site Manager site %s/%s rolled", siteName, siteID))
	}
	return err
}

// GetSiteOTP retrieves the OTP and its expiration time for a given site UUID from Site Manager.
func GetSiteOTP(ctx context.Context, logger zerolog.Logger, uuid, url string) (*string, *time.Time, error) {
	getURL := siteManagerClient.GetSiteURL(url, api.GetSiteRouteName, uuid)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, getURL, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to create request object for Site Manager")
		return nil, nil, err
	}
	c, err := siteManagerClient.request(req, logger)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Site from Site Manager")
		return nil, nil, err
	}

	var siteResp types.SiteGetResponse
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

// CreateSite creates a site in Site Manager.
// It accepts the minimal fields needed to create a site.
func CreateSite(ctx context.Context, logger zerolog.Logger, siteUUID, name, provider, fcOrg, url string) error {
	scr := &types.SiteCreateRequest{
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
	createURL := siteManagerClient.GetSiteURL(url, api.CreateSiteRouteName)
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, createURL, b)
	if err != nil {
		logger.Error().Err(err).Msg("failed to create request object for Site Manager")
		return err
	}
	_, err = siteManagerClient.request(req, logger)
	if err == nil {
		logger.Info().Msg(fmt.Sprintf("Site Manager Site: %s/%s created", name, siteUUID))
	}
	return err
}

// DeleteSite deletes a site from Site Manager using its UUID.
func DeleteSite(ctx context.Context, logger zerolog.Logger, uuid, url string) error {
	deleteURL := siteManagerClient.GetSiteURL(url, api.DeleteSiteRouteName, uuid)
	req, err := http.NewRequestWithContext(ctx, http.MethodDelete, deleteURL, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to create request object for Site Manager")
		return err
	}

	_, err = siteManagerClient.request(req, logger)
	if err == nil {
		logger.Info().Msg(fmt.Sprintf("Site Manager Site: %s deleted", uuid))
	}
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
