// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package elektra

import (
	"crypto/rand"
	"crypto/rsa"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/base64"
	"encoding/pem"
	"errors"
	"html/template"
	"math/big"
	"net"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"gopkg.in/yaml.v2"

	cutils "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/coregrpc"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/conftypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
	"github.com/rs/zerolog/log"

	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
	bootstraptypes "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes/bootstrap"
	workflowtypes "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes/workflow"
)

var (
	// NOTE: These values must match values in test NICo server in elektra-nico-lib

	// DefaultTestVpcID is the default VPC ID for testing
	DefaultTestVpcID = "00000000-0000-4000-8000-000000000000"
	// DefaultTestNetworkSegmentID is the default NetworkSegment ID for testing
	DefaultTestNetworkSegmentID = "00000000-0000-4000-9000-000000000000"
	// DefaultTestTenantKeysetID is the default TenantKeyset ID for testing
	DefaultTestTenantKeysetID = "00000000-0000-4000-a000-000000000000"
	// DefaultTestIBParitionID is the default IBPartition ID for testing
	DefaultTestIBParitionID = "00000000-0000-4000-b000-000000000000"

	// Counters to track various failure/success counts
	wflowGrpcFail = 0
	wflowGrpcSucc = 0
	wflowActFail  = uint64(0)
	wflowActSucc  = uint64(0)
	wflowPubFail  = uint64(0)
	wflowPubSucc  = uint64(0)
)

// Test Elektra objects
var testElektra *Elektra
var testElektraTypes *elektratypes.Elektra

const bootStrapSecretTemplate = `
apiVersion: v1
data:
  site-uuid: {{.SiteID}}
  otp: {{.OTP}}
  creds-url: {{.SiteManagerCredsURL}}
  cacert: {{.SiteManagerCACert}}
kind: Secret
metadata:
  name: bootstrapInfo
type: Opaque
`

const (
	caKeyFileName     = "ca.key"
	testOTPBootstrap  = "test-otp"
	testOTPDownloaded = "test-otp-downloaded"
)

// bootstrapSecretData is a struct for the bootstrap secret
type bootstrapSecretData struct {
	SiteID              string
	OTP                 string
	SiteManagerCredsURL string
	SiteManagerCACert   string
}

// checkGrpcState checks the state of the NICo gRPC connection
func checkGrpcState(stats *workflowtypes.MgrState) {
	fail := int(coregrpc.ManagerAccess.Data.EB.Managers.CoreGrpc.State.GrpcFail.Load())
	if wflowGrpcFail != fail {
		log.Info().Msgf("wflowGrpcFail: %v, state fail: %v ", wflowGrpcFail, fail)
		panic("wflowGrpcFail ctr incorrect")
	}
	succ := int(coregrpc.ManagerAccess.Data.EB.Managers.CoreGrpc.State.GrpcSucc.Load())
	if wflowGrpcSucc != succ {
		log.Info().Msgf("wflowGrpcSucc: %v, state succ %v", wflowGrpcSucc, succ)
		panic("wflowGrpcSucc ctr incorrect")
	}
	state := uint64(coregrpc.ManagerAccess.Data.EB.Managers.CoreGrpc.State.HealthStatus.Load())
	if uint64(computils.CompHealthy) != state {
		log.Info().Msgf("state %v ", state)
		panic("Component not in Healthy State")
	}

	if stats.WflowActFail.Load() != wflowActFail {
		log.Info().Msgf("%v != %v", stats.WflowActFail.Load(), wflowActFail)
		panic("wflowActFail")
	}
	if stats.WflowActSucc.Load() != wflowActSucc {
		log.Info().Msgf("%v != %v", stats.WflowActSucc.Load(), wflowActSucc)
		panic("wflowActSucc")
	}
	if stats.WflowPubSucc.Load() != wflowPubSucc {
		log.Info().Msgf("%v != %v", stats.WflowPubSucc.Load(), wflowPubSucc)
		panic("wflowPubSucc")
	}
	if stats.WflowPubFail.Load() != wflowPubFail {
		log.Info().Msgf("%v != %v", stats.WflowPubFail.Load(), wflowPubFail)
		panic("wflowPubFail")
	}
}

// SetupTestCA generates a test CA certificate and key.
func SetupTestCA(t *testing.T, caCertPath string) string {
	caKeyPath := filepath.Join(filepath.Dir(caCertPath), caKeyFileName)

	// Generate CA private key
	caPrivateKey, err := rsa.GenerateKey(rand.Reader, 2048)
	assert.NoError(t, err)

	// Create a CA certificate template
	caTemplate := x509.Certificate{
		SerialNumber: big.NewInt(1),
		Subject: pkix.Name{
			Organization: []string{"Test CA"},
		},
		NotBefore:             time.Now(),
		NotAfter:              time.Now().Add(365 * 24 * time.Hour),
		KeyUsage:              x509.KeyUsageCertSign | x509.KeyUsageDigitalSignature,
		ExtKeyUsage:           []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth, x509.ExtKeyUsageClientAuth},
		IsCA:                  true,
		BasicConstraintsValid: true,
	}

	// Self-sign the CA certificate
	caCertBytes, err := x509.CreateCertificate(rand.Reader, &caTemplate, &caTemplate, &caPrivateKey.PublicKey, caPrivateKey)
	assert.NoError(t, err)

	// Write the CA private key to file
	err = os.MkdirAll(filepath.Dir(caCertPath), 0755)
	assert.NoError(t, err)

	caKeyFile, err := os.Create(caKeyPath)
	assert.NoError(t, err)
	defer caKeyFile.Close()
	pem.Encode(caKeyFile, &pem.Block{Type: "RSA PRIVATE KEY", Bytes: x509.MarshalPKCS1PrivateKey(caPrivateKey)})

	// Write the CA certificate to file
	caCertFile, err := os.Create(caCertPath)
	assert.NoError(t, err)
	defer caCertFile.Close()
	pem.Encode(caCertFile, &pem.Block{Type: "CERTIFICATE", Bytes: caCertBytes})

	return caCertPath
}

// SetupTestCerts generates a test server/client certificate and key signed by the provided CA.
func SetupTestCerts(t *testing.T, clientCertPath string, clientKeyPath string, caCertPath string) (string, string) {
	// Load CA
	caCertPEM, err := os.ReadFile(caCertPath)
	assert.NoError(t, err)
	caCertBlock, _ := pem.Decode(caCertPEM)
	caCert, err := x509.ParseCertificate(caCertBlock.Bytes)
	assert.NoError(t, err)

	caKeyPath := filepath.Join(filepath.Dir(caCertPath), caKeyFileName)

	caKeyPEM, err := os.ReadFile(caKeyPath)
	assert.NoError(t, err)
	caKeyBlock, _ := pem.Decode(caKeyPEM)
	caPrivateKey, err := x509.ParsePKCS1PrivateKey(caKeyBlock.Bytes)
	assert.NoError(t, err)

	// Generate keypair for test server/client
	privatekey, err := rsa.GenerateKey(rand.Reader, 2048)
	assert.NoError(t, err)
	publickey := &privatekey.PublicKey

	// Create certificate template to be signed by CA
	certTemplate := x509.Certificate{
		SerialNumber: big.NewInt(2),
		Subject: pkix.Name{
			CommonName:   "localhost",
			Organization: []string{"Test Organization"},
		},
		NotBefore:             time.Now(),
		NotAfter:              time.Now().AddDate(5, 0, 0),
		KeyUsage:              x509.KeyUsageDigitalSignature | x509.KeyUsageKeyEncipherment,
		ExtKeyUsage:           []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		IPAddresses:           []net.IP{net.ParseIP("127.0.0.1")},
		BasicConstraintsValid: true,
	}

	// Create the server/client certificate signed by CA
	certBytes, err := x509.CreateCertificate(rand.Reader, &certTemplate, caCert, publickey, caPrivateKey)
	assert.NoError(t, err)

	// Encode and save the server/client private key
	err = os.MkdirAll(filepath.Dir(clientKeyPath), 0755)
	assert.NoError(t, err)

	keyFile, err := os.Create(clientKeyPath)
	assert.NoError(t, err)
	defer keyFile.Close()
	pem.Encode(keyFile, &pem.Block{Type: "RSA PRIVATE KEY", Bytes: x509.MarshalPKCS1PrivateKey(privatekey)})

	// Encode and save the server/client certificate
	err = os.MkdirAll(filepath.Dir(clientCertPath), 0755)
	assert.NoError(t, err)

	certFile, err := os.Create(clientCertPath)
	assert.NoError(t, err)
	defer certFile.Close()
	pem.Encode(certFile, &pem.Block{Type: "CERTIFICATE", Bytes: certBytes})

	return clientKeyPath, clientCertPath
}

// SetupTestBootstrapSecret generates a test bootstrap secret
func SetupTestBootstrapSecret(t *testing.T, conf *conftypes.Config) string {
	// Generate Site Manager CA cert
	siteManagerCACertPath := SetupTestCA(t, "/tmp/site-manager/certs/ca.crt")
	// Read the Site Manager CA cert
	siteManagerCACert, err := os.ReadFile(siteManagerCACertPath)

	assert.NoError(t, err)

	data := bootstrapSecretData{
		SiteID:              conf.Temporal.ClusterID,
		OTP:                 testOTPBootstrap,
		SiteManagerCredsURL: "https://sitemgr.cloud-site-manager:8100/v1/sitecreds",
		SiteManagerCACert:   base64.StdEncoding.EncodeToString(siteManagerCACert),
	}

	// Parse the template
	tmpl, err := template.New("bootstrapSecret").Parse(bootStrapSecretTemplate)
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to parse bootstrap secret template")
	}

	// Execute the template, write to file
	err = os.MkdirAll(filepath.Dir(conf.BootstrapSecret), 0755)
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to create bootstrap secret directory")
	}

	secretFilePath := filepath.Join(conf.BootstrapSecret, "secret.yaml")

	bootstrapSecretFile, err := os.Create(secretFilePath)
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to create bootstrap secret file")
	}
	err = tmpl.Execute(bootstrapSecretFile, data)
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to execute bootstrap secret template")
	}

	return secretFilePath
}

// TestInitElektra initializes a test version of the Site Agent
func TestInitElektra(t *testing.T) {
	if testElektra != nil {
		return
	}
	os.Setenv("CORE_GRPC_CERT_CHECK_INTERVAL", "1") // set this to check if certs were rotated every second to help with unit tests
	defer os.Unsetenv("CORE_GRPC_CERT_CHECK_INTERVAL")

	// Initialize test Site Agent
	log.Info().Msg("Elektra: Initializing test Site Agent")

	// Initialize Site Agent Data Structures
	testElektraTypes = elektratypes.NewElektraTypes()

	// Initialize test Site Agent API
	api, initErr := NewElektraAPI(testElektraTypes, true)
	if initErr != nil {
		log.Fatal().Err(initErr).Msg("Elektra: Failed to initialize test Site Agent API")
	} else {
		log.Info().Msg("Elektra: Successfully initialized test Site Agent API")
	}

	// Config has been initialized here
	// Generate Core gRPC CA and client certs
	coreGrpcCACertPath := SetupTestCA(t, testElektraTypes.Conf.CoreGrpc.ServerCAPath)
	coreGrpcKeyPath, coreGrpcCertPath := SetupTestCerts(t, testElektraTypes.Conf.CoreGrpc.ClientCertPath, testElektraTypes.Conf.CoreGrpc.ClientKeyPath,
		coreGrpcCACertPath)

	assert.Equal(t, coreGrpcCACertPath, testElektraTypes.Conf.CoreGrpc.ServerCAPath)
	assert.Equal(t, coreGrpcKeyPath, testElektraTypes.Conf.CoreGrpc.ClientKeyPath)
	assert.Equal(t, coreGrpcCertPath, testElektraTypes.Conf.CoreGrpc.ClientCertPath)

	// Generate and set Temporal access certs
	temporalCACertPath := SetupTestCA(t, testElektraTypes.Conf.Temporal.GetTemporalCACertFullPath())
	assert.Equal(t, temporalCACertPath, testElektraTypes.Conf.Temporal.GetTemporalCACertFullPath())

	temporalKeyPath, temporalCertPath := SetupTestCerts(t, testElektraTypes.Conf.Temporal.GetTemporalClientCertFullPath(), testElektraTypes.Conf.Temporal.GetTemporalClientKeyFullPath(),
		temporalCACertPath)

	assert.Equal(t, temporalKeyPath, testElektraTypes.Conf.Temporal.GetTemporalClientKeyFullPath())
	assert.Equal(t, temporalCertPath, testElektraTypes.Conf.Temporal.GetTemporalClientCertFullPath())

	// Write OTP to Temporal directory
	err := os.WriteFile(testElektraTypes.Conf.Temporal.GetTemporalCertOTPFullPath(), []byte(testOTPDownloaded), 0644)
	require.NoError(t, err)

	// Generate bootstrap secret
	bootstrapSecretFilePath := SetupTestBootstrapSecret(t, testElektraTypes.Conf)

	err = simulateMountedSecretFile(t, bootstrapSecretFilePath)
	if err != nil {
		log.Fatal().Err(err).Msg("Elektra: Failed to simulate mounted secret file")
	}
	// Initialize test Site Agent Managers
	api.Init()
	api.Start()

	testElektra = api
}

func simulateMountedSecretFile(t *testing.T, secretFilePath string) error {
	result := &struct {
		Data bootstraptypes.SecretConfig `yaml:"data"`
	}{}

	secretDir := filepath.Dir(secretFilePath)

	log.Info().Msgf("Test mounted secret file path: %s", secretFilePath)

	cfgFile, err := os.ReadFile(secretFilePath)
	if err != nil {
		return err
	}

	log.Info().Msgf("Read bootstrap secret file: %v bytes", len(cfgFile))

	err = yaml.Unmarshal(cfgFile, result)
	if err != nil {
		return err
	}
	bCfg := &result.Data

	// Return error if credentials are not available
	if bCfg.CACert == "" || bCfg.CredsURL == "" || bCfg.OTP == "" || bCfg.UUID == "" {
		return errors.New("could not read bootstrap secret from file")
	}

	log.Info().Msg("Successfully read bootstrap secret from file:")
	log.Info().Msgf("UUID: %v", bCfg.UUID)
	log.Info().Msgf("OTP: %v", cutils.RedactSecret(bCfg.OTP, cutils.SecretLogPrefixLen))
	log.Info().Msgf("CACert: %v", cutils.RedactSecret(bCfg.CACert, cutils.CertLogPrefixLen))
	log.Info().Msgf("CredsURL: %v", bCfg.CredsURL)

	err = os.WriteFile(filepath.Join(secretDir, bootstraptypes.TagUUID), []byte(bCfg.UUID), 0644)
	if err != nil {
		return err
	}

	err = os.WriteFile(filepath.Join(secretDir, bootstraptypes.TagOTP), []byte(bCfg.OTP), 0644)
	if err != nil {
		return err
	}

	err = os.WriteFile(filepath.Join(secretDir, bootstraptypes.TagCACert), []byte(bCfg.CACert), 0644)
	if err != nil {
		return err
	}

	err = os.WriteFile(filepath.Join(secretDir, bootstraptypes.TagCredsURL), []byte(bCfg.CredsURL), 0644)
	if err != nil {
		return err
	}

	log.Info().Msg("Successfully wrote bootstrap secrets to file")

	return nil
}
