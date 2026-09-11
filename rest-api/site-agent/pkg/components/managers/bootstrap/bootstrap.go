// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package bootstrap

import (
	"bytes"
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/json"
	"encoding/pem"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptrace"
	"net/url"
	"os"
	"path/filepath"
	"time"

	"github.com/prometheus/client_golang/prometheus"

	"github.com/rs/zerolog/log"

	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/conftypes"
	bootstraptypes "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes/bootstrap"
	"gopkg.in/fsnotify.v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes"
	coreV1Types "k8s.io/client-go/kubernetes/typed/core/v1"
	"k8s.io/client-go/rest"
	"k8s.io/client-go/tools/clientcmd"

	"go.opentelemetry.io/contrib/instrumentation/net/http/httptrace/otelhttptrace"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/codes"
)

var (
	// ErrInvalidBootstrapSecret invalid bootstrap secret
	ErrInvalidBootstrapSecret = errors.New("invalid bootstrap secret")
)

const (
	// MetricCredDnloadAttempt - Metric Cred Dnload Attempt
	MetricCredDnloadAttempt = "credentials_download_attempted"
	// MetricCredDnloadSucc - Metric Cred Dnload Succ
	MetricCredDnloadSucc = "credentials_download_succeeded"
)

// newBootstrapConfig creates the Configuration required for fetching Credentials
func newBootstrapConfig(dir string) error {
	log.Info().Msgf("Bootstrap: Reading from secret: %v", dir)

	bCfg := ManagerAccess.Data.EB.Managers.Bootstrap.Config
	bSecretFiles := ManagerAccess.Data.EB.Managers.Bootstrap.Secretfiles

	file := dir + bootstraptypes.TagUUID
	bSecretFiles[file] = true
	readBytes, err := os.ReadFile(file)
	if err != nil {
		return err
	}
	bCfg.UUID = string(readBytes)

	file = dir + bootstraptypes.TagOTP
	bSecretFiles[file] = true
	readBytes, err = os.ReadFile(file)
	if err != nil {
		return err
	}
	bCfg.OTP = string(readBytes)

	file = dir + bootstraptypes.TagCredsURL
	bSecretFiles[file] = true
	readBytes, err = os.ReadFile(file)
	if err != nil {
		return err
	}
	bCfg.CredsURL = string(readBytes)

	file = dir + bootstraptypes.TagCACert
	bSecretFiles[file] = true
	readBytes, err = os.ReadFile(file)
	if err != nil {
		return err
	}
	bCfg.CACert = string(readBytes)

	// Return error if credentials are not available
	if bCfg.CACert == "" || bCfg.CredsURL == "" || bCfg.OTP == "" || bCfg.UUID == "" {
		return ErrInvalidBootstrapSecret
	}
	log.Info().Msgf("Bootstrap: Read %v %v %v %v", bCfg.UUID, bCfg.OTP, bCfg.CredsURL, bCfg.CACert)

	return nil
}

/*
 * First check in cluster config
 * If not check the environment variable for KUBECONFIG
 * If not set, choose ~/.kube/config
 * If not found, error!
 */
func initK8sClient(ns string) coreV1Types.SecretInterface {
	config, err := rest.InClusterConfig()
	if err != nil {
		log.Info().Msg("Bootstrap: InClusterConfig failed, try OutOfCluster config")
		kubeconfig := os.Getenv("KUBECONFIG")
		if kubeconfig == "" {
			log.Info().Msg("Bootstrap: $KUBECONFIG is not set. Using $HOME/.kube/config (if present)")
			kubeconfig = filepath.Join(
				os.Getenv("HOME"), ".kube", "config",
			)
		}
		if kubeconfig == "" {
			err = fmt.Errorf("Bootstrap: could not find kubeconfig")
			panic(err.Error())
		}
		config, err = clientcmd.BuildConfigFromFlags("", kubeconfig)
		if err != nil {
			panic(err.Error())
		}
	}
	clientset, err := kubernetes.NewForConfig(config)
	if err != nil {
		panic(err.Error())
	}

	return clientset.CoreV1().Secrets(ns)
}

// Init - initialize the bootstrap manager
func (bs *BoostrapAPI) Init() {
	ManagerAccess.Data.EB.Log.Info().Msg("Boostrap: Initializing the Site bootstrap manager")

	// Only master pod of the statefulset should run the bootstrap
	if !ManagerAccess.Conf.EB.IsMasterPod {
		return
	}

	if ManagerAccess.Conf.EB.DisableBootstrap {
		return
	}

	prometheus.MustRegister(
		prometheus.NewCounterFunc(prometheus.CounterOpts{
			Namespace: ManagerAccess.Conf.EB.MetricsNamespace,
			Name:      MetricCredDnloadAttempt,
			Help:      "Credentials download attempted for Site Agent",
		},
			func() float64 {
				return float64(ManagerAccess.Data.EB.Managers.Bootstrap.State.DownloadAttempted.Load())
			}))

	prometheus.MustRegister(
		prometheus.NewCounterFunc(prometheus.CounterOpts{
			Namespace: ManagerAccess.Conf.EB.MetricsNamespace,
			Name:      MetricCredDnloadSucc,
			Help:      "Credentials download succeeded for Site Agent",
		},
			func() float64 {
				return float64(ManagerAccess.Data.EB.Managers.Bootstrap.State.DownloadSucceeded.Load())
			}))

	err := newBootstrapConfig(ManagerAccess.Conf.EB.BootstrapSecret)
	if err != nil {
		ManagerAccess.Data.EB.Log.Fatal().Msgf("Boostrap: error %v", err.Error())
	}

	if ManagerAccess.Conf.EB.RunningIn == conftypes.RunningInK8s {
		ManagerAccess.Data.EB.Managers.Bootstrap.Secret = initK8sClient(ManagerAccess.Conf.EB.PodNamespace)
	}
}

// watchBootstrapFile watch on the secret path
func (bs *BoostrapAPI) watchBootstrapFile() {
	err := bs.watchSecretFiles(ManagerAccess.Data.EB.Managers.Bootstrap.Secretfiles, &ManagerAccess.Conf.EB.BootstrapSecret)
	if err != nil {
		log.Panic().Msg(err.Error())
	}
}

// watchSecretFiles watch on the secret file
func (bs *BoostrapAPI) watchSecretFiles(files map[string]bool, path *string) error {
	log.Info().Msgf("Bootstrap: Watching secret %v", *path)

	// Create a new watcher.
	w, err := fsnotify.NewWatcher()
	if err != nil {
		log.Error().Msgf("Bootstrap: creating a new watcher: %v", err)
		return err
	}
	defer w.Close()

	// Watch the directory, not the file itself.
	err = w.Add(*path)
	if err != nil {
		log.Error().Msgf("Bootstrap: add file to watcher: %v", err)
		return err
	}

	for {
		select {
		// Read from Errors.
		case err, ok := <-w.Errors:
			if !ok { // Channel was closed (i.e. Watcher.Close() was called).
				return nil
			}
			log.Error().Msgf("Bootstrap: watcher closed: %v", err.Error())
			return err
		// Read from Events.
		case e, ok := <-w.Events:
			if !ok { // Channel was closed (i.e. Watcher.Close() was called).
				return nil
			}

			_, ok = files[e.Name]
			if !ok {
				continue
			}
			log.Info().Msgf("Bootstrap: File updated %s", e.String())
			bs.DownloadAndStoreCreds(nil)
			log.Info().Msgf("Bootstrap: back to Watching secret %s ", e.String())
		}
	}
}

// Start bootstrap
func (bs *BoostrapAPI) Start() {
	if !ManagerAccess.Conf.EB.IsMasterPod {
		return
	}
	if ManagerAccess.Conf.EB.DisableBootstrap {
		return
	}

	log.Info().Msgf("Bootstrap: trigger workflow")
	bs.DownloadAndStoreCreds(nil)
	go bs.watchBootstrapFile()
}

// GetState - handle http request
func (bs *BoostrapAPI) GetState() []string {
	bt := ManagerAccess.Data.EB.Managers.Bootstrap
	var strs []string
	strs = append(strs, fmt.Sprintln("Creds Download Attempted: ", bt.State.DownloadAttempted.Load()))
	strs = append(strs, fmt.Sprintln("Creds Download Succeeded: ", bt.State.DownloadSucceeded.Load()))
	strs = append(strs, fmt.Sprintln("URL: ", bt.Config.CredsURL))
	strs = append(strs, fmt.Sprintln("OTP: ", bt.Config.OTP))
	strs = append(strs, fmt.Sprintln("UUID: ", bt.Config.UUID))

	return strs
}

// DownloadAndStoreCreds - Download and Store Temporal credentials
func (bs *BoostrapAPI) DownloadAndStoreCreds(otpOverride []byte) error {
	// Check if certs for this OTP have already been downloaded
	bCfg := ManagerAccess.Data.EB.Managers.Bootstrap.Config

	otpFilePath := ManagerAccess.Conf.EB.Temporal.GetTemporalCertOTPFullPath()
	otpBytes, err := os.ReadFile(otpFilePath)
	if err != nil {
		log.Warn().Err(err).Msg("Bootstrap: Failed to read stored OTP file")
		// If file doesn't exist yet, that's ok on first run
		if !os.IsNotExist(err) {
			return err
		}
		otpBytes = []byte{}
	}

	// If an OTP override is provided and not nil, use it and update the config
	if otpOverride != nil {
		otpBytes = otpOverride
		bCfg.OTP = string(otpBytes) // Update the config with the override value
		log.Info().Msgf("Bootstrap: Using OTP override")
	}

	// If no OTP override, check if credentials have already been downloaded for the OTP in the config
	fileOTP := string(otpBytes)
	configOTP := bCfg.OTP
	if otpOverride == nil && len(fileOTP) > 0 && fileOTP == configOTP {
		log.Info().Msgf("Bootstrap: Credentials already downloaded for OTP (file matches config)")
		return nil
	}
	log.Info().Msgf("Bootstrap: Credentials need to be downloaded for OTP (file OTP len=%d, config OTP len=%d, match=%v)",
		len(fileOTP), len(configOTP), fileOTP == configOTP)

	bw := ManagerAccess.Data.EB.Managers.Bootstrap.State
	// Keep track of events
	bw.DownloadAttempted.Inc()

	ctx := context.Background()
	ctx, span := otel.Tracer("elektra-site-agent").Start(ctx, "Bootstrap")
	defer span.End()

	// Proceed to download credentials with the updated OTP
	credsResponse, err := bs.downloadCredentials(ctx)
	if err != nil {
		span.SetStatus(codes.Error, err.Error())
		log.Info().Msgf("Bootstrap: Download Credentials Response %v", err.Error())
		return err
	}
	err = bs.storeCredentials(ctx, credsResponse)
	if err != nil {
		span.SetStatus(codes.Error, err.Error())
		log.Info().Msgf("Bootstrap: Store Credentials %v", err.Error())
		return err
	}
	span.SetStatus(codes.Ok, "Bootstrap: DownloadSucceeded")

	// Keep track of credential download success
	bw.DownloadSucceeded.Inc()
	log.Info().Msgf("Bootstrap: DownloadSucceeded %v", bw.DownloadSucceeded)
	return nil
}

func saveToFile(credsResponse *bootstraptypes.SiteCredsResponse) error {
	pathOTP := ManagerAccess.Conf.EB.Temporal.GetTemporalCertOTPFullPath()
	pathCaCert := ManagerAccess.Conf.EB.Temporal.GetTemporalCACertFullPath()
	pathCert := ManagerAccess.Conf.EB.Temporal.GetTemporalClientCertFullPath()
	pathKey := ManagerAccess.Conf.EB.Temporal.GetTemporalClientKeyFullPath()

	bCfg := ManagerAccess.Data.EB.Managers.Bootstrap.Config

	// Write OTP file and ensure it's synced to disk to avoid race conditions
	otpData := []byte(bCfg.OTP)
	err := os.WriteFile(pathOTP, otpData, 0644)
	if err != nil {
		return err
	}
	// Open and sync the OTP file to ensure it's flushed to disk
	// Note: without this there is a 10-15% flakiness on tests...
	otpFile, err := os.OpenFile(pathOTP, os.O_RDWR, 0644)
	if err == nil {
		otpFile.Sync()
		otpFile.Close()
	}

	err = os.WriteFile(pathCaCert, []byte(credsResponse.CACertificate), 0644)
	if err != nil {
		return err
	}
	err = os.WriteFile(pathCert, []byte(credsResponse.Certificate), 0644)
	if err != nil {
		return err
	}
	err = os.WriteFile(pathKey, []byte(credsResponse.Key), 0644)
	if err != nil {
		return err
	}
	log.Info().Msgf("Bootstrap: saved to disk")

	return nil
}

// StoreCredentials for updating secrets
func (bs *BoostrapAPI) storeCredentials(ctx context.Context, credsResponse *bootstraptypes.SiteCredsResponse) error {
	ctx, span := otel.Tracer("elektra-site-agent").Start(ctx, "Bootstrap-store")
	defer span.End()
	if credsResponse == nil {
		return fmt.Errorf("Bootstrap: credsResponse is nil")
	}
	if ManagerAccess.Conf.EB.RunningIn != conftypes.RunningInK8s {
		err := saveToFile(credsResponse)
		if err != nil {
			return err
		}
		return nil
	}
	secretIf := ManagerAccess.Data.EB.Managers.Bootstrap.Secret
	if secretIf == nil {
		return fmt.Errorf("Bootstrap: secretIf is nil")
	}
	// Update a secret via Update
	secret, err := secretIf.Get(ctx, ManagerAccess.Conf.EB.TemporalSecret, metav1.GetOptions{})
	if err != nil {
		log.Error().Msgf("Bootstrap: error reading temporal cert secret: %v", err.Error())
		return err
	}
	log.Info().Msgf("Bootstrap: successfully read temporal cert secret: %v", ManagerAccess.Conf.EB.TemporalSecret)

	if secret.Data == nil {
		secret.Data = make(map[string][]byte)
	}

	bCfg := ManagerAccess.Data.EB.Managers.Bootstrap.Config

	secret.StringData = make(map[string]string)
	secret.StringData["otp"] = bCfg.OTP
	secret.StringData["cacertificate"] = credsResponse.CACertificate
	secret.StringData["certificate"] = credsResponse.Certificate
	secret.StringData["key"] = credsResponse.Key

	_, err = secretIf.Update(ctx, secret, metav1.UpdateOptions{})
	if err != nil {
		log.Error().Msgf("Bootstrap: Failed to write Temporal certs: %v", err.Error())
		return err
	}

	log.Info().Msg("Bootstrap: Updated Temporal cert in secret")

	return nil
}

// DownloadCredentials Read-in bootstrap file, connect to url and download credentials
func (bs *BoostrapAPI) downloadCredentials(ctx context.Context) (*bootstraptypes.SiteCredsResponse, error) {
	bCfg := ManagerAccess.Data.EB.Managers.Bootstrap.Config
	bReq := &bootstraptypes.SecretReq{
		UUID: bCfg.UUID,
		OTP:  bCfg.OTP,
	}
	m, err := json.Marshal(bReq)
	if err != nil {
		log.Error().Msgf("Bootstrap: req %v", err.Error())
		return nil, err
	}
	log.Info().Msgf("Bootstrap: body %v", string(m))
	ctx, span := otel.Tracer("elektra-site-agent").Start(ctx, "Bootstrap-client")
	span.SetAttributes(attribute.String("url", bCfg.CredsURL))
	defer span.End()

	u, err := url.Parse(bCfg.CredsURL)
	if err != nil {
		log.Error().Msgf("Bootstrap: url parse %v", err.Error())
		return nil, err
	}
	log.Info().Msgf("Bootstrap: hostname %v, %v", string(u.Hostname()), bCfg.CredsURL)

	caCertPool := x509.NewCertPool()
	caCertPool.AppendCertsFromPEM([]byte(bCfg.CACert))
	insecureSkipVerify := false
	if u.Hostname() == "::" {
		log.Info().Msgf("Bootstrap: local host so skip verification")
		insecureSkipVerify = true
	}

	client := &http.Client{
		Timeout: 1 * time.Minute,
		Transport: &http.Transport{
			TLSClientConfig: &tls.Config{
				RootCAs:            caCertPool,
				ServerName:         u.Hostname(),
				InsecureSkipVerify: insecureSkipVerify,
			},
		},
	}
	ctx = httptrace.WithClientTrace(ctx, otelhttptrace.NewClientTrace(ctx))
	req, err := http.NewRequestWithContext(ctx, "POST", bCfg.CredsURL, bytes.NewReader(m))
	if err != nil {
		log.Error().Msgf("Bootstrap: new req failed %v", err.Error())
		return nil, err
	}
	req.Header.Add("Accept", "application/json")
	req.Header.Add("Content-Type", "application/json")
	otelhttptrace.Inject(ctx, req)

	resp, err := computils.RetryWithExponentialBackoff(client, req, m)
	if err != nil {
		log.Error().Msgf("Bootstrap: client connection failed: %v", err.Error())
		return nil, err
	}
	defer resp.Body.Close()
	bodyBytes, err := io.ReadAll(resp.Body)
	if err != nil {
		log.Error().Msgf("Bootstrap: resp read failed: %v", err.Error())
		return nil, err
	}

	credsResponse := &bootstraptypes.SiteCredsResponse{}
	err = json.Unmarshal(bodyBytes, credsResponse)
	if err != nil {
		log.Error().Msgf("Bootstrap: API Response unmarshal %+v", err.Error())
		return nil, err
	}

	block, _ := pem.Decode([]byte(credsResponse.CACertificate))
	if block == nil {
		log.Error().Msgf("Bootstrap: failed to decode certificate PEM")
		return nil, fmt.Errorf("failed to decode certificate PEM CACertificate %v", credsResponse.CACertificate)
	}

	_, err = x509.ParseCertificate(block.Bytes)
	if err != nil {
		log.Error().Err(err).Msgf("Bootstrap: failed to parse certificate")
		return nil, fmt.Errorf("failed to parse certificate %w", err)
	}

	return credsResponse, nil
}
