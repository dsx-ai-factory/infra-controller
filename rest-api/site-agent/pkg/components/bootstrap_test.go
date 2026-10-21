// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package elektra

import (
	"context"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/bootstrap"
	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
	bootstraptypes "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes/bootstrap"
	sitemgr "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/sitemgr"
	log "github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// Test_Bootstrap - test the bootstrap registeration
func Test_Bootstrap(t *testing.T) {
	TestInitElektra(t)
	t.Log(bootstrap.ManagerAccess.Conf.EB.BootstrapSecret)

	tcs := []struct {
		descr string
	}{
		{
			descr: "read",
		},
		{
			descr: "touch",
		},
		{
			descr: "noop",
		},
		{
			descr: "get",
		},
		{
			descr: "saStatus",
		},
	}
	for _, tc := range tcs {
		t.Run(tc.descr, func(t *testing.T) {
			switch tc.descr {
			case "read":
				log.Info().Msg("Test Bootstrap read Start")
				bCfg := bootstrap.ManagerAccess.Data.EB.Managers.Bootstrap.GetConfig()
				t.Log(bCfg.CACert)
				t.Log(bCfg.CredsURL)
				t.Log(bCfg.OTP)
				t.Log(bCfg.UUID)
				assert.NotEqual(t, bCfg.CACert, "")
				assert.NotEqual(t, bCfg.CredsURL, "")
				assert.NotEqual(t, bCfg.OTP, "")
				assert.NotEqual(t, bCfg.UUID, "")
				log.Info().Msg("Test Bootstrap Read End")
			case "touch":
				log.Info().Msg("Test Bootstrap touch Start")
				// A new OTP in the bootstrap Secret reaches reloadBootstrapCredentials
				// only through the watcher that Start runs. The download then fails
				// against the test creds-url, but the attempt is counted first.
				w := bootstrap.ManagerAccess.Data.EB.Managers.Bootstrap.State
				otpPath := filepath.Join(bootstrap.ManagerAccess.Conf.EB.BootstrapSecret, bootstraptypes.TagOTP)
				originalOTP, err := os.ReadFile(otpPath)
				require.NoError(t, err)
				attemptCount := w.DownloadAttempted.Load()

				require.NoError(t, os.WriteFile(otpPath, []byte("touch-test-otp"), 0644))
				assert.Eventually(t, func() bool {
					return w.DownloadAttempted.Load() > attemptCount
				}, 5*time.Second, 20*time.Millisecond, "watcher did not reload the bootstrap Secret")

				// The original OTP matches the active configuration, so restoring it
				// is skipped without another attempt.
				require.NoError(t, os.WriteFile(otpPath, originalOTP, 0644))
				log.Info().Msg("Test Bootstrap touch End")
			case "noop":
				log.Info().Msg("Test Bootstrap noop Start")
				bootstrap.ManagerAccess.Data.EB.Managers.Bootstrap.State.DownloadAttempted.Store(0)
				for i := 0; i <= 1; i++ {
					time.Sleep(10 * time.Millisecond)
					assert.Equal(t, 0,
						int(bootstrap.ManagerAccess.Data.EB.Managers.Bootstrap.State.DownloadAttempted.Load()))
				}
				log.Info().Msg("Test Bootstrap noop End")
			case "get":
				log.Info().Msg("Test Bootstrap get Start")
				s, err := sitemgr.TestManagerCreateSite()
				assert.Nil(t, err)
				assert.NotNil(t, s)

				// Save current values
				bootstrapManager := bootstrap.ManagerAccess.Data.EB.Managers.Bootstrap
				bCfg := bootstrapManager.GetConfig()
				bCfg.OTP = s.UUID1OTP
				bCfg.CredsURL = s.MgrURL + "/v1/sitecreds"
				bCfg.UUID = sitemgr.Testuuid1
				bootstrapManager.ReplaceConfig(bCfg)

				t.Logf("CredsURL: %s", bCfg.CredsURL)

				state := bootstrap.ManagerAccess.Data.EB.Managers.Workflow.State
				state.ConnectionAttempted.Store(0)
				err = bootstrap.ManagerAccess.API.Bootstrap.DownloadAndStoreCreds(context.Background(), nil)
				if err != nil {
					t.Error(err.Error())
				}
				assert.Nil(t, err)
				s.Teardown()
				for i := 0; i <= 10; i++ {
					time.Sleep(10 * time.Millisecond)
					if state.ConnectionAttempted.Load() > 0 {
						t.Logf("Certs update detected")
						break
					}
				}
				attemptCount := state.ConnectionAttempted.Load()
				assert.GreaterOrEqual(t, attemptCount, uint64(1))

				pathOTP := bootstrap.ManagerAccess.Conf.EB.Temporal.GetTemporalCertOTPFullPath()
				pathCaCert := bootstrap.ManagerAccess.Conf.EB.Temporal.GetTemporalCACertFullPath()
				pathCert := bootstrap.ManagerAccess.Conf.EB.Temporal.GetTemporalClientCertFullPath()
				pathKey := bootstrap.ManagerAccess.Conf.EB.Temporal.GetTemporalClientKeyFullPath()

				assert.FileExists(t, pathOTP)
				assert.FileExists(t, pathCaCert)
				assert.FileExists(t, pathCert)
				assert.FileExists(t, pathKey)

				otpBytes, err := os.ReadFile(pathOTP)
				assert.NoError(t, err)
				assert.Equal(t, s.UUID1OTP, string(otpBytes))

				// Try to download again, should be skipped since OTP is same
				err = bootstrap.ManagerAccess.API.Bootstrap.DownloadAndStoreCreds(context.Background(), nil)
				assert.NoError(t, err)

				assert.Equal(t, attemptCount, state.ConnectionAttempted.Load())

				log.Info().Msg("Test Bootstrap get End")
			case "saStatus":
				resp, err := http.Get("http://localhost:" + computils.StatusPort() + computils.SiteStatus)
				if err != nil {
					t.Log(err)
					return
				}
				defer resp.Body.Close()
				body, err := io.ReadAll(resp.Body)
				if err != nil {
					t.Log(err)
				}
				t.Log(string(body))
			}
		})
	}
}
