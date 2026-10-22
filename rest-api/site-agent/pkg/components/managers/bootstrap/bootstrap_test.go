// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package bootstrap

import (
	"bytes"
	"context"
	"encoding/json"
	"encoding/pem"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"gopkg.in/fsnotify.v1"
	coreV1Types "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes/fake"

	Manager "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/managerapi"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/conftypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes"
	bootstraptypes "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes/bootstrap"
)

// testOTP matches what Site Manager issues: 20 random bytes in URL-safe base64.
const testOTP = "8Nn5Qk0mVQqHqk2hXwfXQz1Yk5A="

func TestReadBootstrapConfigUsesOneProjectedSecretRevision(t *testing.T) {
	root := t.TempDir()
	oldRevision := filepath.Join(root, "..old")
	newRevision := filepath.Join(root, "..new")
	require.NoError(t, os.Mkdir(oldRevision, 0700))
	require.NoError(t, os.Mkdir(newRevision, 0700))

	writeConfig := func(dir string, config bootstraptypes.SecretConfig) {
		t.Helper()
		for name, contents := range map[string]string{
			bootstraptypes.TagUUID:     config.UUID,
			bootstraptypes.TagOTP:      config.OTP,
			bootstraptypes.TagCredsURL: config.CredsURL,
			bootstraptypes.TagCACert:   config.CACert,
		} {
			require.NoError(t, os.WriteFile(filepath.Join(dir, name), []byte(contents), 0600))
		}
	}
	oldConfig := bootstraptypes.SecretConfig{
		UUID:     "old-site",
		OTP:      "old-otp",
		CredsURL: "https://old.example.test",
		CACert:   "old-ca",
	}
	newConfig := bootstraptypes.SecretConfig{
		UUID:     "new-site",
		OTP:      "new-otp",
		CredsURL: "https://new.example.test",
		CACert:   "new-ca",
	}
	writeConfig(oldRevision, oldConfig)
	writeConfig(newRevision, newConfig)
	require.NoError(t, os.Symlink(filepath.Base(newRevision), filepath.Join(root, kubernetesSecretDataSymlink)))

	for _, tag := range []string{
		bootstraptypes.TagUUID,
		bootstraptypes.TagOTP,
		bootstraptypes.TagCredsURL,
		bootstraptypes.TagCACert,
	} {
		targetRevision := kubernetesSecretDataSymlink
		if tag == bootstraptypes.TagUUID {
			// Simulate a symlink view changing between separate key reads.
			targetRevision = filepath.Base(oldRevision)
		}
		require.NoError(t, os.Symlink(filepath.Join(targetRevision, tag), filepath.Join(root, tag)))
	}

	config, files, err := readBootstrapConfig(root)
	require.NoError(t, err)
	assert.Equal(t, newConfig, config)
	assert.ElementsMatch(t, []string{
		filepath.Join(root, bootstraptypes.TagUUID),
		filepath.Join(root, bootstraptypes.TagOTP),
		filepath.Join(root, bootstraptypes.TagCredsURL),
		filepath.Join(root, bootstraptypes.TagCACert),
	}, files)
}

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

	bCfg := ManagerAccess.Data.EB.Managers.Bootstrap.GetConfig()
	assert.Equal(t, testOTP, bCfg.OTP)

	assert.NotContains(t, logged.String(), testOTP)
	assert.NotContains(t, logged.String(), caCert)
	assert.Contains(t, logged.String(), siteID)
	assert.Contains(t, logged.String(), credsURL)
}

func TestIsBootstrapSecretUpdate(t *testing.T) {
	dir := t.TempDir()
	otpFile := filepath.Join(dir, bootstraptypes.TagOTP)
	files := map[string]bool{otpFile: true}
	tests := []struct {
		name  string
		event fsnotify.Event
		want  bool
	}{
		{
			name:  "tracked file write",
			event: fsnotify.Event{Name: otpFile, Op: fsnotify.Write},
			want:  true,
		},
		{
			name:  "tracked file metadata change",
			event: fsnotify.Event{Name: otpFile, Op: fsnotify.Chmod},
			want:  true,
		},
		{
			name:  "projected Secret generation swap",
			event: fsnotify.Event{Name: filepath.Join(dir, kubernetesSecretDataSymlink), Op: fsnotify.Create},
			want:  true,
		},
		{
			name:  "projected Secret symlink metadata change",
			event: fsnotify.Event{Name: filepath.Join(dir, kubernetesSecretDataSymlink), Op: fsnotify.Chmod},
			want:  false,
		},
		{
			name:  "projected Secret staging directory",
			event: fsnotify.Event{Name: filepath.Join(dir, "..2026_10_12_20_00_00"), Op: fsnotify.Create},
			want:  false,
		},
		{
			name:  "unrelated file",
			event: fsnotify.Event{Name: filepath.Join(dir, "unrelated"), Op: fsnotify.Write},
			want:  false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, isBootstrapSecretUpdate(tt.event, files, dir))
		})
	}
}

func TestBoostrapAPI_ReloadBootstrapCredentials(t *testing.T) {
	const (
		siteID      = "d2f4b0c6-6f1e-4a0e-9f5a-0b6a6f4c1e77"
		otherSiteID = "5b0e7c1a-2d3f-4e8a-9c6b-7a1d2e3f4a5b"
		rotatedOTP  = "Zr2pW9xT4mLq8vB1nC6yD0eF3gH="
	)
	tests := []struct {
		name           string
		updatedSiteID  string
		downloadFails  bool
		alreadyApplied bool
		wantErr        string
		wantAttempts   uint64
		wantSucceeded  uint64
		wantRequests   int
		wantStoredOTP  string
	}{
		{
			name:          "reloads a same-Site OTP rotation",
			updatedSiteID: siteID,
			wantAttempts:  1,
			wantSucceeded: 1,
			wantRequests:  1,
			wantStoredOTP: rotatedOTP,
		},
		{
			name:          "rejects a Site re-pair",
			updatedSiteID: otherSiteID,
			wantErr:       "bootstrap Secret Site ID changed",
		},
		{
			name:          "download failure preserves the active configuration",
			updatedSiteID: siteID,
			downloadFails: true,
			wantErr:       "missing protocol scheme",
			wantAttempts:  1,
		},
		{
			name:           "skips a projected Secret already applied by Cloud rotation",
			updatedSiteID:  siteID,
			alreadyApplied: true,
			wantAttempts:   1,
			wantSucceeded:  1,
			wantStoredOTP:  testOTP,
		},
	}
	previousAccess := ManagerAccess
	t.Cleanup(func() { ManagerAccess = previousAccess })
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			requests := make(chan bootstraptypes.SecretReq, 1)
			var caCert string
			server := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				var request bootstraptypes.SecretReq
				if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
					http.Error(w, err.Error(), http.StatusBadRequest)
					return
				}
				requests <- request
				w.Header().Set("Content-Type", "application/json")
				_ = json.NewEncoder(w).Encode(bootstraptypes.SiteCredsResponse{
					Key:           "client-key",
					Certificate:   "client-certificate",
					CACertificate: caCert,
				})
			}))
			t.Cleanup(server.Close)
			caCert = string(pem.EncodeToMemory(&pem.Block{
				Type:  "CERTIFICATE",
				Bytes: server.Certificate().Raw,
			}))

			secretDir := t.TempDir()
			temporalDir := t.TempDir()
			require.NoError(t, os.Mkdir(filepath.Join(temporalDir, "ca"), 0755))
			require.NoError(t, os.Mkdir(filepath.Join(temporalDir, "client"), 0755))

			writeConfig := func(config bootstraptypes.SecretConfig) {
				t.Helper()
				for name, contents := range map[string]string{
					bootstraptypes.TagUUID:     config.UUID,
					bootstraptypes.TagOTP:      config.OTP,
					bootstraptypes.TagCredsURL: config.CredsURL,
					bootstraptypes.TagCACert:   config.CACert,
				} {
					require.NoError(t, os.WriteFile(filepath.Join(secretDir, name), []byte(contents), 0600))
				}
			}

			initialConfig := bootstraptypes.SecretConfig{
				UUID:     siteID,
				OTP:      testOTP,
				CredsURL: "https://old.invalid/v1/sitecreds",
				CACert:   "old-ca",
			}
			writeConfig(initialConfig)
			ManagerAccess = &Manager.ManagerAccess{
				Conf: &Manager.ManagerConf{EB: &conftypes.Config{
					BootstrapSecret: secretDir,
					RunningIn:       conftypes.RunningInDocker,
					Temporal: conftypes.TemporalConfig{
						TemporalCertPath: temporalDir,
					},
				}},
				Data: &Manager.ManagerData{
					EB: &elektratypes.Elektra{
						Managers: &managertypes.Managers{
							Bootstrap: bootstraptypes.NewBootstrapInstance(),
						},
					},
				},
			}
			require.NoError(t, newBootstrapConfig(secretDir))

			updatedConfig := bootstraptypes.SecretConfig{
				UUID:     tt.updatedSiteID,
				OTP:      rotatedOTP,
				CredsURL: server.URL,
				CACert:   caCert,
			}
			if tt.downloadFails {
				updatedConfig.CredsURL = "://invalid"
			}
			writeConfig(updatedConfig)

			bootstrap := ManagerAccess.Data.EB.Managers.Bootstrap
			if tt.alreadyApplied {
				bootstrap.ReplaceConfig(updatedConfig)
				bootstrap.Registration.AddOTP(rotatedOTP)
				bootstrap.State.DownloadAttempted.Store(1)
				bootstrap.State.DownloadSucceeded.Store(1)
				require.NoError(t, os.WriteFile(filepath.Join(temporalDir, bootstraptypes.TagOTP), []byte(testOTP), 0600))
			}

			err := (&BoostrapAPI{}).reloadBootstrapCredentials(secretDir)
			if tt.wantErr != "" {
				require.ErrorContains(t, err, tt.wantErr)
			} else {
				require.NoError(t, err)
			}

			wantConfig := updatedConfig
			if tt.wantErr != "" {
				wantConfig = initialConfig
			}
			assert.Equal(t, wantConfig, bootstrap.GetConfig())
			assert.Equal(t, tt.wantRequests, len(requests))
			assert.Equal(t, tt.wantAttempts, bootstrap.State.DownloadAttempted.Load())
			assert.Equal(t, tt.wantSucceeded, bootstrap.State.DownloadSucceeded.Load())
			if tt.wantErr != "" {
				assert.Error(t, ManagerAccess.Data.EB.Managers.Bootstrap.Registration.Check(tt.updatedSiteID, rotatedOTP))
				return
			}

			require.NoError(t, bootstrap.Registration.Check(siteID, rotatedOTP))

			if tt.wantRequests == 1 {
				request := <-requests
				assert.Equal(t, siteID, request.UUID)
				assert.Equal(t, rotatedOTP, request.OTP)
			}
			if tt.wantStoredOTP != "" {
				storedOTP, readErr := os.ReadFile(filepath.Join(temporalDir, bootstraptypes.TagOTP))
				require.NoError(t, readErr)
				assert.Equal(t, tt.wantStoredOTP, string(storedOTP))
			}
		})
	}
}

func TestBoostrapAPI_DownloadAndStoreCreds(t *testing.T) {
	const (
		siteID     = "d2f4b0c6-6f1e-4a0e-9f5a-0b6a6f4c1e77"
		rotatedOTP = "Zr2pW9xT4mLq8vB1nC6yD0eF3gH="
		newerOTP   = "Kt7uY2iO5pA9sD4fG8hJ1kL6zXc="
	)
	const temporalSecretName = "temporal-cert"
	// Kubernetes mode, where the projected OTP file can lag behind the Secret, so
	// only the Secret read from the API server shows what was stored.
	tests := []struct {
		name         string
		publishedOTP string
		storedOTP    string
		wantErr      string
		wantAttempts uint64
		wantOTP      string
	}{
		{
			name:         "credentials stored for the override skip the download",
			publishedOTP: testOTP,
			storedOTP:    rotatedOTP,
			wantOTP:      rotatedOTP,
		},
		{
			// Init publishes the bootstrap Secret's OTP before any download, as
			// after a restart between Site Manager consuming the OTP and the store.
			name:         "published override OTP without stored credentials still downloads",
			publishedOTP: rotatedOTP,
			storedOTP:    testOTP,
			wantErr:      "missing protocol scheme",
			wantAttempts: 1,
			wantOTP:      rotatedOTP,
		},
	}
	previousAccess := ManagerAccess
	t.Cleanup(func() { ManagerAccess = previousAccess })
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			secrets := fake.NewClientset().CoreV1().Secrets("default")
			_, err := secrets.Create(context.Background(), &coreV1Types.Secret{
				ObjectMeta: metav1.ObjectMeta{Name: temporalSecretName},
				Data:       map[string][]byte{"otp": []byte(tt.storedOTP)},
			}, metav1.CreateOptions{})
			require.NoError(t, err)

			bootstrap := bootstraptypes.NewBootstrapInstance()
			bootstrap.Secret = secrets
			bootstrap.ReplaceConfig(bootstraptypes.SecretConfig{
				UUID:     siteID,
				OTP:      tt.publishedOTP,
				CredsURL: "://invalid",
				CACert:   "active-ca",
			})
			ManagerAccess = &Manager.ManagerAccess{
				Conf: &Manager.ManagerConf{EB: &conftypes.Config{
					RunningIn:      conftypes.RunningInK8s,
					TemporalSecret: temporalSecretName,
				}},
				Data: &Manager.ManagerData{
					EB: &elektratypes.Elektra{
						Managers: &managertypes.Managers{
							Bootstrap: bootstrap,
						},
					},
				},
			}

			err = (&BoostrapAPI{}).DownloadAndStoreCreds(context.Background(), []byte(rotatedOTP))
			if tt.wantErr != "" {
				require.ErrorContains(t, err, tt.wantErr)
			} else {
				require.NoError(t, err)
			}

			assert.Equal(t, tt.wantOTP, bootstrap.GetConfig().OTP)
			assert.Equal(t, tt.wantAttempts, bootstrap.State.DownloadAttempted.Load())
			assert.Zero(t, bootstrap.State.DownloadSucceeded.Load())
		})
	}

	t.Run("caller cancellation stops stalled credential download", func(t *testing.T) {
		requestStarted := make(chan struct{})
		releaseRequest := make(chan struct{})
		defer close(releaseRequest)
		server := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			close(requestStarted)
			<-releaseRequest
		}))
		t.Cleanup(server.Close)
		caCert := string(pem.EncodeToMemory(&pem.Block{
			Type:  "CERTIFICATE",
			Bytes: server.Certificate().Raw,
		}))

		temporalDir := t.TempDir()
		otpPath := filepath.Join(temporalDir, bootstraptypes.TagOTP)
		require.NoError(t, os.WriteFile(otpPath, []byte(testOTP), 0600))

		bootstrap := bootstraptypes.NewBootstrapInstance()
		activeConfig := bootstraptypes.SecretConfig{
			UUID:     siteID,
			OTP:      rotatedOTP,
			CredsURL: server.URL,
			CACert:   caCert,
		}
		bootstrap.ReplaceConfig(activeConfig)
		ManagerAccess = &Manager.ManagerAccess{
			Conf: &Manager.ManagerConf{EB: &conftypes.Config{
				RunningIn: conftypes.RunningInDocker,
				Temporal: conftypes.TemporalConfig{
					TemporalCertPath: temporalDir,
				},
			}},
			Data: &Manager.ManagerData{
				EB: &elektratypes.Elektra{
					Managers: &managertypes.Managers{
						Bootstrap: bootstrap,
					},
				},
			},
		}

		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()
		downloadDone := make(chan error, 1)
		go func() {
			downloadDone <- (&BoostrapAPI{}).DownloadAndStoreCreds(ctx, []byte(newerOTP))
		}()

		select {
		case <-requestStarted:
		case <-time.After(5 * time.Second):
			t.Fatal("credential request did not start")
		}
		cancel()
		select {
		case err := <-downloadDone:
			require.ErrorIs(t, err, context.Canceled)
		case <-time.After(time.Second):
			t.Fatal("credential download did not stop after cancellation")
		}
		assert.Equal(t, activeConfig, bootstrap.GetConfig())
		assert.Equal(t, uint64(1), bootstrap.State.DownloadAttempted.Load())
		assert.Zero(t, bootstrap.State.DownloadSucceeded.Load())
	})
}

func TestBootstrap_RunCredentialUpdate(t *testing.T) {
	initialConfig := bootstraptypes.SecretConfig{UUID: "initial-site", OTP: testOTP}
	updatedConfig := bootstraptypes.SecretConfig{UUID: "initial-site", OTP: "updated-otp"}
	bootstrap := bootstraptypes.NewBootstrapInstance()
	bootstrap.ReplaceConfig(initialConfig)

	updateStarted := make(chan struct{})
	releaseUpdate := make(chan struct{})
	updateDone := make(chan error, 1)
	go func() {
		updateDone <- bootstrap.RunCredentialUpdate(context.Background(), func(bootstraptypes.SecretConfig) (bootstraptypes.SecretConfig, error) {
			close(updateStarted)
			<-releaseUpdate
			return updatedConfig, nil
		})
	}()
	<-updateStarted

	configRead := make(chan bootstraptypes.SecretConfig, 1)
	go func() {
		configRead <- bootstrap.GetConfig()
	}()
	select {
	case config := <-configRead:
		assert.Equal(t, initialConfig, config)
	case <-time.After(time.Second):
		close(releaseUpdate)
		t.Fatal("GetConfig blocked during credential update")
	}

	close(releaseUpdate)
	require.NoError(t, <-updateDone)
	assert.Equal(t, updatedConfig, bootstrap.GetConfig())
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
