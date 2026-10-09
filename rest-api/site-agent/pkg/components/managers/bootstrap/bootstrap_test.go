// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package bootstrap

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	Manager "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/managerapi"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/conftypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes"
	bootstraptypes "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes/bootstrap"
)

// testOTP matches what Site Manager issues: 20 random bytes in URL-safe base64.
const testOTP = "8Nn5Qk0mVQqHqk2hXwfXQz1Yk5A="

// TestNewBootstrapConfig guards the startup record every Site Agent
// initialization emits, which has to identify the Site and the secret it read
// without carrying the OTP that is still live at that point.
func TestNewBootstrapConfig(t *testing.T) {
	const (
		siteID   = "d2f4b0c6-6f1e-4a0e-9f5a-0b6a6f4c1e77"
		credsURL = "https://sitemgr.nico-system.svc/v1/sitecreds"
	)
	// A real Site Manager CA runs past a thousand characters, so the body has to
	// outrun the logged prefix for the assertions below to mean anything.
	caCert := "-----BEGIN CERTIFICATE-----\n" + strings.Repeat("c2l0ZS1jYQ", 100) +
		"\n-----END CERTIFICATE-----"

	dir := t.TempDir()
	for name, contents := range map[string]string{
		bootstraptypes.TagUUID:     siteID,
		bootstraptypes.TagOTP:      testOTP,
		bootstraptypes.TagCredsURL: credsURL,
		bootstraptypes.TagCACert:   caCert,
	} {
		require.NoError(t, os.WriteFile(filepath.Join(dir, name), []byte(contents), 0600))
	}

	ManagerAccess = &Manager.ManagerAccess{
		Data: &Manager.ManagerData{
			EB: &elektratypes.Elektra{
				Managers: &managertypes.Managers{
					Bootstrap: bootstraptypes.NewBootstrapInstance(),
				},
			},
		},
	}

	var logged bytes.Buffer
	restore := log.Logger
	log.Logger = zerolog.New(&logged)
	t.Cleanup(func() { log.Logger = restore })

	err := newBootstrapConfig(dir + string(os.PathSeparator))
	require.NoError(t, err)

	bCfg := ManagerAccess.Data.EB.Managers.Bootstrap.Config
	assert.Equal(t, testOTP, bCfg.OTP)

	assert.NotContains(t, logged.String(), testOTP)
	assert.NotContains(t, logged.String(), caCert)
	assert.Contains(t, logged.String(), siteID)
	assert.Contains(t, logged.String(), credsURL)
}

func TestBoostrapAPI_CheckRegistration(t *testing.T) {
	const (
		siteID      = "d2f4b0c6-6f1e-4a0e-9f5a-0b6a6f4c1e77"
		otherSiteID = "5b0e7c1a-2d3f-4e8a-9c6b-7a1d2e3f4a5b"
		rotatedOTP  = "Zr2pW9xT4mLq8vB1nC6yD0eF3gH="
		repairOTP   = "Kt7uY2iO5pA9sD4fG8hJ1kL6zXc="
	)
	tests := []struct {
		name string
		// loaded is false on a pod that does not run the bootstrap.
		loaded bool
		// rotated is an OTP the Site Agent wrote back through OTP rotation.
		rotated string
		// siteID and otp are what site-registration holds when liveness checks it.
		siteID  string
		otp     string
		wantErr string
	}{
		{name: "rotated OTP not mounted yet", loaded: true, rotated: rotatedOTP, siteID: siteID, otp: testOTP},
		{
			name:    "re-paired to another Site",
			loaded:  true,
			siteID:  otherSiteID,
			otp:     repairOTP,
			wantErr: "site-registration holds Site ID " + otherSiteID + ", not " + siteID,
		},
		{
			name:    "re-paired with a new OTP",
			loaded:  true,
			siteID:  siteID,
			otp:     repairOTP,
			wantErr: "site-registration holds an OTP the Site Agent did not apply",
		},
		{name: "bootstrap not run", siteID: otherSiteID, otp: repairOTP},
	}
	previousAccess := ManagerAccess
	t.Cleanup(func() { ManagerAccess = previousAccess })
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dir := t.TempDir() + string(os.PathSeparator)
			writeRegistration := func(siteID, otp string) {
				t.Helper()
				for name, contents := range map[string]string{
					bootstraptypes.TagUUID:     siteID,
					bootstraptypes.TagOTP:      otp,
					bootstraptypes.TagCredsURL: "https://sitemgr.nico-system.svc/v1/sitecreds",
					bootstraptypes.TagCACert:   "-----BEGIN CERTIFICATE-----\nc2l0ZS1jYQ\n-----END CERTIFICATE-----",
				} {
					require.NoError(t, os.WriteFile(dir+name, []byte(contents), 0600))
				}
			}
			ManagerAccess = &Manager.ManagerAccess{
				Conf: &Manager.ManagerConf{EB: &conftypes.Config{BootstrapSecret: dir}},
				Data: &Manager.ManagerData{
					EB: &elektratypes.Elektra{
						Managers: &managertypes.Managers{
							Bootstrap: bootstraptypes.NewBootstrapInstance(),
						},
					},
				},
			}

			writeRegistration(siteID, testOTP)
			if tt.loaded {
				require.NoError(t, newBootstrapConfig(dir))
			}
			if tt.rotated != "" {
				ManagerAccess.Data.EB.Managers.Bootstrap.Registration.AddOTP(tt.rotated)
			}
			writeRegistration(tt.siteID, tt.otp)

			err := (&BoostrapAPI{}).CheckRegistration()
			if tt.wantErr == "" {
				assert.NoError(t, err)
				return
			}
			assert.EqualError(t, err, tt.wantErr)
		})
	}
}
