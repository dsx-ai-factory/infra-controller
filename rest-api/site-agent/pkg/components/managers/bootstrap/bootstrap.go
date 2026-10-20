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

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	cutils "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
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
	"go.opentelemetry.io/otel/attribute"
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
	// Kubernetes projects Secret keys through the ..data symlink and atomically
	// replaces that symlink on updates, so watching only the key paths misses changes.
	kubernetesSecretDataSymlink = "..data"
	// credentialUpdateTimeout bounds each serialized download and store operation,
	// including projected Secret reloads, so external I/O cannot hold the update lock indefinitely.
	credentialUpdateTimeout = 90 * time.Second
)

func resolveBootstrapRevision(dir string) (string, error) {
	dataSymlink := filepath.Join(dir, kubernetesSecretDataSymlink)
	if _, err := os.Lstat(dataSymlink); err == nil {
		return filepath.EvalSymlinks(dataSymlink)
	} else if !os.IsNotExist(err) {
		return "", err
	}
	return dir, nil
}

func readBootstrapConfig(dir string) (bootstraptypes.SecretConfig, []string, error) {
	log.Info().Msgf("Bootstrap: Reading from secret: %v", dir)

	configDir, err := resolveBootstrapRevision(dir)
	if err != nil {
		return bootstraptypes.SecretConfig{}, nil, err
	}

	config := bootstraptypes.SecretConfig{}
	fields := []struct {
		tag   string
		value *string
	}{
		{
			tag:   bootstraptypes.TagUUID,
			value: &config.UUID,
		},
		{
			tag:   bootstraptypes.TagOTP,
			value: &config.OTP,
		},
		{
			tag:   bootstraptypes.TagCredsURL,
			value: &config.CredsURL,
		},
		{
			tag:   bootstraptypes.TagCACert,
			value: &config.CACert,
		},
	}
	files := make([]string, 0, len(fields))
	for _, field := range fields {
		file := filepath.Join(dir, field.tag)
		readBytes, err := os.ReadFile(filepath.Join(configDir, field.tag))
		if err != nil {
			return bootstraptypes.SecretConfig{}, nil, err
		}
		*field.value = string(readBytes)
		files = append(files, file)
	}

	if config.CACert == "" || config.CredsURL == "" || config.OTP == "" || config.UUID == "" {
		return bootstraptypes.SecretConfig{}, nil, ErrInvalidBootstrapSecret
	}

	return config, files, nil
}

// newBootstrapConfig creates the configuration required for fetching credentials.
func newBootstrapConfig(dir string) error {
	config, files, err := readBootstrapConfig(dir)
	if err != nil {
		return err
	}
	bootstrap := ManagerAccess.Data.EB.Managers.Bootstrap
	bootstrap.ReplaceConfig(config)
	for _, file := range files {
		bootstrap.Secretfiles[file] = true
	}
	bootstrap.Registration.Load(config.UUID, config.OTP)
	// The OTP is named to report that it was read, with no value of any kind. It
	// is a live credential from the moment the secret is read until the handshake
	// consumes it.
	log.Info().
		Str("site_id", config.UUID).
		Str("credentials_url", config.CredsURL).
		Str("ca_certificate", cutils.RedactSecret(config.CACert, cutils.CertLogPrefixLen)).
		Msg("Bootstrap: configuration loaded, OTP read")

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
			err = fmt.Errorf("bootstrap: could not find kubeconfig")
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

func isBootstrapSecretUpdate(event fsnotify.Event, files map[string]bool, dir string) bool {
	eventPath := filepath.Clean(event.Name)
	if files[eventPath] {
		return true
	}

	return event.Op&fsnotify.Create != 0 &&
		filepath.Dir(eventPath) == filepath.Clean(dir) &&
		filepath.Base(eventPath) == kubernetesSecretDataSymlink
}

func (bs *BoostrapAPI) reloadBootstrapCredentials(dir string) error {
	bootstrap := ManagerAccess.Data.EB.Managers.Bootstrap
	ctx, cancel := context.WithTimeout(context.Background(), credentialUpdateTimeout)
	defer cancel()
	var publishedConfig bootstraptypes.SecretConfig
	var reloaded bool
	err := bootstrap.RunCredentialUpdate(ctx, func(currentConfig bootstraptypes.SecretConfig) (bootstraptypes.SecretConfig, error) {
		candidate, _, err := readBootstrapConfig(dir)
		if err != nil {
			return currentConfig, err
		}
		if candidate == currentConfig {
			log.Info().Msg("Bootstrap: Credentials already applied for the projected Secret")
			return currentConfig, nil
		}

		currentSiteID := currentConfig.UUID
		if currentSiteID != "" && candidate.UUID != currentSiteID {
			return currentConfig, fmt.Errorf("bootstrap Secret Site ID changed from %s to %s", currentSiteID, candidate.UUID)
		}

		err = bs.downloadAndStoreCreds(ctx, candidate)
		if err != nil {
			return currentConfig, err
		}

		bootstrap.Registration.AddOTP(candidate.OTP)
		publishedConfig = candidate
		reloaded = true
		return candidate, nil
	})
	if err != nil {
		return err
	}
	if !reloaded {
		return nil
	}

	log.Info().
		Str("site_id", publishedConfig.UUID).
		Str("credentials_url", publishedConfig.CredsURL).
		Str("ca_certificate", cutils.RedactSecret(publishedConfig.CACert, cutils.CertLogPrefixLen)).
		Msg("Bootstrap: configuration reloaded, OTP read")
	return nil
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

			if !isBootstrapSecretUpdate(e, files, *path) {
				continue
			}
			log.Info().Msgf("Bootstrap: File updated %s", e.String())
			if err := bs.reloadBootstrapCredentials(*path); err != nil {
				log.Error().Err(err).Msg("Bootstrap: failed to reload credentials")
			}
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
	// DownloadAndStoreCreds logs its own failures, and the Site Agent keeps its existing certificates.
	_ = bs.DownloadAndStoreCreds(context.Background(), nil)
	go bs.watchBootstrapFile()
}

// CheckRegistration returns an error once site-registration holds a Site ID or OTP the
// Site Agent did not apply, as after a re-pair. Same-Site OTP rotations are recorded only
// after the new credentials are stored successfully. A file it cannot read back is not
// treated as a re-pair.
func (bs *BoostrapAPI) CheckRegistration() error {
	dir := ManagerAccess.Conf.EB.BootstrapSecret
	configDir, err := resolveBootstrapRevision(dir)
	if err != nil {
		return nil
	}
	siteID, err := os.ReadFile(filepath.Join(configDir, bootstraptypes.TagUUID))
	if err != nil {
		return nil
	}
	otp, err := os.ReadFile(filepath.Join(configDir, bootstraptypes.TagOTP))
	if err != nil {
		return nil
	}
	return ManagerAccess.Data.EB.Managers.Bootstrap.Registration.Check(string(siteID), string(otp))
}

// DownloadAndStoreCreds - Download and Store Temporal credentials
func (bs *BoostrapAPI) DownloadAndStoreCreds(ctx context.Context, otpOverride []byte) error {
	ctx, cancel := context.WithTimeout(ctx, credentialUpdateTimeout)
	defer cancel()
	bootstrap := ManagerAccess.Data.EB.Managers.Bootstrap
	return bootstrap.RunCredentialUpdate(ctx, func(candidate bootstraptypes.SecretConfig) (bootstraptypes.SecretConfig, error) {
		if otpOverride != nil {
			candidate.OTP = string(otpOverride)
			log.Info().Msg("Bootstrap: Using OTP override")
		}

		err := bs.downloadAndStoreCreds(ctx, candidate)
		if err != nil {
			return candidate, err
		}

		if otpOverride != nil {
			bootstrap.Registration.AddOTP(candidate.OTP)
		}
		return candidate, nil
	})
}

// getStoredCredentialOTP returns the OTP that storeCredentials last stored, or an
// empty string before the first store. In Kubernetes it reads the Temporal cert
// Secret from the API server, because the projected OTP file can still hold the
// previous OTP after an update. The published configuration is not enough
// either: Init publishes the bootstrap Secret's OTP before any download.
func (bs *BoostrapAPI) getStoredCredentialOTP(ctx context.Context) (string, error) {
	if ManagerAccess.Conf.EB.RunningIn != conftypes.RunningInK8s {
		otpBytes, err := os.ReadFile(ManagerAccess.Conf.EB.Temporal.GetTemporalCertOTPFullPath())
		if os.IsNotExist(err) {
			return "", nil
		}
		if err != nil {
			return "", err
		}
		return string(otpBytes), nil
	}

	secretIf := ManagerAccess.Data.EB.Managers.Bootstrap.Secret
	if secretIf == nil {
		return "", fmt.Errorf("bootstrap: secretIf is nil")
	}
	secret, err := secretIf.Get(ctx, ManagerAccess.Conf.EB.TemporalSecret, metav1.GetOptions{})
	if err != nil {
		return "", err
	}
	return string(secret.Data["otp"]), nil
}

func (bs *BoostrapAPI) downloadAndStoreCreds(ctx context.Context, config bootstraptypes.SecretConfig) error {
	storedOTP, err := bs.getStoredCredentialOTP(ctx)
	if err != nil {
		log.Warn().Err(err).Msg("Bootstrap: Failed to read stored OTP")
		return err
	}

	configOTP := config.OTP
	if len(storedOTP) > 0 && storedOTP == configOTP {
		log.Info().Msgf("Bootstrap: Credentials already stored for OTP")
		return nil
	}
	log.Info().Msgf("Bootstrap: Credentials need to be downloaded for OTP (stored OTP len=%d, config OTP len=%d)",
		len(storedOTP), len(configOTP))

	bw := ManagerAccess.Data.EB.Managers.Bootstrap.State
	// Keep track of events
	bw.DownloadAttempted.Inc()

	ctx, span := cotel.StartSpan(ctx, "Bootstrap")
	defer span.End()

	// Proceed to download credentials with the updated OTP
	credsResponse, err := bs.downloadCredentials(ctx, config)
	if err != nil {
		cotel.RecordError(span, err)
		// A stale OTP is the usual cause, so the prefix identifies which one the
		// Site sent.
		log.Error().Err(err).Msgf("Bootstrap: Download Credentials failed for Site %v from %v with OTP %v",
			config.UUID, config.CredsURL, cutils.RedactSecret(config.OTP, cutils.SecretLogPrefixLen))
		return err
	}
	err = bs.storeCredentials(ctx, credsResponse, config.OTP)
	if err != nil {
		cotel.RecordError(span, err)
		log.Info().Msgf("Bootstrap: Store Credentials %v", err.Error())
		return err
	}
	cotel.RecordSuccess(span)

	// Keep track of credential download success
	bw.DownloadSucceeded.Inc()
	log.Info().Msgf("Bootstrap: DownloadSucceeded %v", bw.DownloadSucceeded)
	return nil
}

func saveToFile(credsResponse *bootstraptypes.SiteCredsResponse, otp string) error {
	pathOTP := ManagerAccess.Conf.EB.Temporal.GetTemporalCertOTPFullPath()
	pathCaCert := ManagerAccess.Conf.EB.Temporal.GetTemporalCACertFullPath()
	pathCert := ManagerAccess.Conf.EB.Temporal.GetTemporalClientCertFullPath()
	pathKey := ManagerAccess.Conf.EB.Temporal.GetTemporalClientKeyFullPath()

	// Write OTP file and ensure it's synced to disk to avoid race conditions
	otpData := []byte(otp)
	err := os.WriteFile(pathOTP, otpData, 0644)
	if err != nil {
		return err
	}
	// Open and sync the OTP file to ensure it's flushed to disk
	// Note: without this there is a 10-15% flakiness on tests...
	otpFile, err := os.OpenFile(pathOTP, os.O_RDWR, 0644)
	if err == nil {
		err = otpFile.Sync()
		if err != nil {
			log.Warn().Err(err).Msg("Bootstrap: failed to sync OTP file to disk")
		}
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
func (bs *BoostrapAPI) storeCredentials(ctx context.Context, credsResponse *bootstraptypes.SiteCredsResponse, otp string) error {
	ctx, span := cotel.StartSpan(ctx, "Bootstrap-store")
	defer span.End()
	if credsResponse == nil {
		return fmt.Errorf("bootstrap: credsResponse is nil")
	}
	if ManagerAccess.Conf.EB.RunningIn != conftypes.RunningInK8s {
		err := saveToFile(credsResponse, otp)
		if err != nil {
			return err
		}
		return nil
	}
	secretIf := ManagerAccess.Data.EB.Managers.Bootstrap.Secret
	if secretIf == nil {
		return fmt.Errorf("bootstrap: secretIf is nil")
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

	secret.StringData = make(map[string]string)
	secret.StringData["otp"] = otp
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
func (bs *BoostrapAPI) downloadCredentials(ctx context.Context, bCfg bootstraptypes.SecretConfig) (*bootstraptypes.SiteCredsResponse, error) {
	bReq := &bootstraptypes.SecretReq{
		UUID: bCfg.UUID,
		OTP:  bCfg.OTP,
	}
	m, err := json.Marshal(bReq)
	if err != nil {
		log.Error().Msgf("Bootstrap: req %v", err.Error())
		return nil, err
	}
	log.Info().Str("site_id", bCfg.UUID).Msg("Bootstrap: requesting credentials")
	ctx, span := cotel.StartSpan(ctx, "Bootstrap-client")
	defer span.End()

	u, err := url.Parse(bCfg.CredsURL)
	if err != nil {
		// url.Error embeds the raw URL, which may carry credentials.
		cotel.RecordError(span, errors.New("invalid credentials endpoint URL"))
		log.Error().Msg("Bootstrap: url parse failed")
		return nil, err
	}

	// CredsURL comes from the bootstrap secret; record only the host and port.
	cotel.SetAttribute(span, attribute.String("server.address", u.Hostname()))
	if port := u.Port(); port != "" {
		cotel.SetAttribute(span, attribute.String("server.port", port))
	}
	log.Info().Str("hostname", u.Hostname()).Msg("Bootstrap: credentials endpoint parsed")

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
		return nil, fmt.Errorf("failed to decode certificate PEM CACertificate %v",
			cutils.RedactSecret(credsResponse.CACertificate, cutils.CertLogPrefixLen))
	}

	_, err = x509.ParseCertificate(block.Bytes)
	if err != nil {
		log.Error().Err(err).Msgf("Bootstrap: failed to parse certificate")
		return nil, fmt.Errorf("failed to parse certificate %w", err)
	}

	return credsResponse, nil
}
