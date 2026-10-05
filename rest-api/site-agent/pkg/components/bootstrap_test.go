// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package elektra

import (
	"os"
	"testing"
	"time"

	"github.com/dsx-ai-factory/infra-controller/rest-api/site-agent/pkg/components/managers/bootstrap"
	computils "github.com/dsx-ai-factory/infra-controller/rest-api/site-agent/pkg/components/utils"
	sitemgr "github.com/dsx-ai-factory/infra-controller/rest-api/site-manager/pkg/sitemgr"
	log "github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
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
				bCfg := bootstrap.ManagerAccess.Data.EB.Managers.Bootstrap.Config
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
				time.Sleep(10 * time.Millisecond)
				w := bootstrap.ManagerAccess.Data.EB.Managers.Bootstrap.State
				files := make([]string, len(bootstrap.ManagerAccess.Data.EB.Managers.Bootstrap.Secretfiles))
				i := 0
				for k := range bootstrap.ManagerAccess.Data.EB.Managers.Bootstrap.Secretfiles {
					files[i] = k
					i++
				}
				NumLoop := 1
				for i := 0; i <= NumLoop; i++ {
					// Update file
					currentTime := time.Now().Local()
					err := os.Chtimes(files[0],
						currentTime, currentTime)
					if err != nil {
						t.Log(err)
					}
					time.Sleep(350 * time.Millisecond)
					assert.GreaterOrEqual(t, int(w.DownloadAttempted.Load()), i+2)
				}
				assert.GreaterOrEqual(t, int(w.DownloadAttempted.Load()), NumLoop+1)
				t.Log(w.DownloadAttempted.Load())
				log.Info().Msg("Test Bootstrap touch Start")
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
				bCfg := bootstrap.ManagerAccess.Data.EB.Managers.Bootstrap.Config
				bCfg.OTP = s.UUID1OTP
				bCfg.CredsURL = s.MgrURL + "/v1/sitecreds"
				bCfg.UUID = sitemgr.Testuuid1

				t.Logf("CredsURL: %s", bCfg.CredsURL)

				state := bootstrap.ManagerAccess.Data.EB.Managers.Workflow.State
				state.ConnectionAttempted.Store(0)
				err = bootstrap.ManagerAccess.API.Bootstrap.DownloadAndStoreCreds(nil)
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
				err = bootstrap.ManagerAccess.API.Bootstrap.DownloadAndStoreCreds(nil)
				assert.NoError(t, err)

				assert.Equal(t, attemptCount, state.ConnectionAttempted.Load())

				log.Info().Msg("Test Bootstrap get End")
			case "saStatus":
				computils.GetSAStatus(computils.SiteStatus)
			}
		})
	}
}
