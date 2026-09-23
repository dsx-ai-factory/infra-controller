// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package bootstrap

import (
	"context"
	"crypto/rand"
	"crypto/rsa"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/base64"
	"encoding/pem"
	"math/big"
	"net"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"

	coreV1Types "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes/fake"

	cutils "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	Manager "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/managerapi"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/conftypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes"
	bootstraptypes "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes/bootstrap"
	workflowtypes "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes/workflow"

	tmocks "go.temporal.io/sdk/mocks"
)

// MockBoostrap is a mock implementation of the BoostrapInterface
type MockBoostrap struct{}

func (m *MockBoostrap) Init()  {}
func (m *MockBoostrap) Start() {}
func (m *MockBoostrap) DownloadAndStoreCreds(otpOverride []byte) error {
	return nil
}
func (m *MockBoostrap) GetState() []string {
	return []string{"state1", "state2"}
}
func (m *MockBoostrap) RegisterSubscriber() error {
	return nil
}

func TestOTPHandler_ReceiveAndSaveOTP(t *testing.T) {
	client := fake.NewSimpleClientset()
	secretInterface := client.CoreV1().Secrets("default")
	otpHandler := &OTPHandler{
		SecretInterface: secretInterface,
	}

	mtc := &tmocks.Client{}

	siteID := "test-site-id"

	ManagerAccess = &Manager.ManagerAccess{
		API: &Manager.ManagerAPI{
			Bootstrap: &MockBoostrap{},
		},
		Conf: &Manager.ManagerConf{
			EB: &conftypes.Config{
				TemporalSecret: "temporal-cert",
				Temporal: conftypes.TemporalConfig{
					ClusterID: siteID,
				},
			},
		},
		Data: &Manager.ManagerData{
			EB: &elektratypes.Elektra{
				Managers: &managertypes.Managers{
					Bootstrap: &bootstraptypes.Bootstrap{
						Config: &bootstraptypes.SecretConfig{
							UUID: siteID,
						},
					},
					Workflow: &workflowtypes.Workflow{
						Temporal: workflowtypes.Temporal{
							Publisher: mtc,
						},
					},
				},
			},
		},
	}

	// Create a mock bootstrap-info secret
	mockOtp := "mockOtp"
	mockOtpB64 := base64.StdEncoding.EncodeToString([]byte(mockOtp))

	mockSecret := &coreV1Types.Secret{
		ObjectMeta: metav1.ObjectMeta{
			Name:      "bootstrap-info",
			Namespace: "default",
		},
		Data: map[string][]byte{
			"otp": []byte(mockOtpB64),
		},
	}
	_, err := client.CoreV1().Secrets("default").Create(context.TODO(), mockSecret, metav1.CreateOptions{})

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
	caCert, err := x509.ParseCertificate(caCertBytes)

	// Generate keypair for test server/client
	privatekey, err := rsa.GenerateKey(rand.Reader, 2048)
	assert.NoError(t, err)
	publickey := &privatekey.PublicKey

	curTime := time.Now().Truncate(time.Second)
	certExpiry := curTime.AddDate(5, 0, 0)

	// Create certificate template to be signed by CA
	certTemplate := x509.Certificate{
		SerialNumber: big.NewInt(2),
		Subject: pkix.Name{
			CommonName:   "localhost",
			Organization: []string{"Test Organization"},
		},
		NotBefore:             time.Now(),
		NotAfter:              certExpiry,
		KeyUsage:              x509.KeyUsageDigitalSignature | x509.KeyUsageKeyEncipherment,
		ExtKeyUsage:           []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		IPAddresses:           []net.IP{net.ParseIP("127.0.0.1")},
		BasicConstraintsValid: true,
	}

	// Create the server/client certificate signed by CA
	certBytes, err := x509.CreateCertificate(rand.Reader, &certTemplate, caCert, publickey, caPrivateKey)
	assert.NoError(t, err)
	pemCertBytes := pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: certBytes})

	mockTemporalSecret := &coreV1Types.Secret{
		ObjectMeta: metav1.ObjectMeta{
			Name:      ManagerAccess.Conf.EB.TemporalSecret,
			Namespace: "default",
		},
		Data: map[string][]byte{
			"certificate": []byte(pemCertBytes),
		},
	}
	_, err = client.CoreV1().Secrets("default").Create(context.TODO(), mockTemporalSecret, metav1.CreateOptions{})

	// Test with a valid OTP
	encryptedOtp := cutils.EncryptData([]byte(mockOtp), ManagerAccess.Conf.EB.Temporal.ClusterID)
	encryptedOtpB64 := base64.StdEncoding.EncodeToString(encryptedOtp)

	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return("test-workflow-id")
	mtc.Mock.On("ExecuteWorkflow", context.Background(), mock.Anything, "UpdateAgentCertExpiry", siteID, certExpiry.UTC()).Return(wrun, nil)

	err = otpHandler.ReceiveAndSaveOTP(context.Background(), encryptedOtpB64)
	if err != nil {
		t.Errorf("Expected no error, got %v", err)
	}
}
