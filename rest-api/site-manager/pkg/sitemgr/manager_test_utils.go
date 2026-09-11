// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sitemgr

import (
	"bytes"
	"context"
	"crypto/tls"
	"encoding/json"
	"encoding/pem"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sync"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/cert-manager/pkg/certs"
	"github.com/NVIDIA/infra-controller/rest-api/cert-manager/pkg/core"
	fakecrdclient "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/client/clientset/versioned/fake"
	crdsv1 "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/crds/v1"
	"github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/types"
	"github.com/gorilla/mux"
)

const (
	// Testuuid1 Test UUID 1
	Testuuid1 = "test-uuid1-1234567890"
	testuuid2 = "test-uuid2-1234567890"
	testuuid3 = "test-uuid3-1234567890"
)

// Suite test instance
type Suite struct {
	sync.Mutex
	l net.Listener
	//srv      *http.Server
	srv           *httptest.Server
	forceErr      bool
	tc            *http.Client
	MgrURL        string
	cancel        context.CancelFunc
	UUID1OTP      string
	certClientDir string
}

var (
	testCACert = `-----BEGIN CERTIFICATE-----
MIID2TCCAsGgAwIBAgIUIVX0I99US3zjd++SfmCfCiN8uOAwDQYJKoZIhvcNAQEL
BQAwfDELMAkGA1UEBhMCVVMxEzARBgNVBAgMCk5ldyBTd2VkZW4xEzARBgNVBAcM
ClN0b2NraG9sbSAxCzAJBgNVBAoMAnV0MQswCQYDVQQLDAJ1dDEVMBMGA1UEAwwM
dW5pdC10ZXN0LWNhMRIwEAYJKoZIhvcNAQkBFgMuLi4wHhcNMjIwODA0MjE0ODUw
WhcNMzIwODAxMjE0ODUwWjB8MQswCQYDVQQGEwJVUzETMBEGA1UECAwKTmV3IFN3
ZWRlbjETMBEGA1UEBwwKU3RvY2tob2xtIDELMAkGA1UECgwCdXQxCzAJBgNVBAsM
AnV0MRUwEwYDVQQDDAx1bml0LXRlc3QtY2ExEjAQBgkqhkiG9w0BCQEWAy4uLjCC
ASIwDQYJKoZIhvcNAQEBBQADggEPADCCAQoCggEBAOGxMNiPxlH+nbSUS99N5L3u
76GI2moSgQNdRc+qzIpzEvJfUuBgeMjzwF3s3Bb/W4SrCWQp3wxWHuWDjRDyGha1
6+pR8yVjLYxYC7Jyu4/+X5L0Fqo7jSAWETUJ+NRFX5/yO/DddmLkvdNd5YYtYx8L
IkGMnSieH286OrC5ofaSZaBErZsKZtlP51n+1iuNYz+VrFT5XK2/aYWUSL1PAe/R
ZmH1i3jrdeWzgIMKPCBcBJwG4xJkY3BcKlk/1QYsnE44D3oNv25IsMA5s6P13VYA
12gFNTBiSqBVlDyVmbH/Z92JqpvaU9brzxHIH2H1uR15TDu5CZMoSPFvPjz0K7sC
AwEAAaNTMFEwHQYDVR0OBBYEFO/GWi+A+NnB3P39XIjCnGzkLQHpMB8GA1UdIwQY
MBaAFO/GWi+A+NnB3P39XIjCnGzkLQHpMA8GA1UdEwEB/wQFMAMBAf8wDQYJKoZI
hvcNAQELBQADggEBAF/ePArhtx0fLU3ad7yNLsWpXbQTWxf6yAPMdI3TF1E68L+A
OinPmWl0Uwr/4eFgA+ijpVcWw+YptlPqCztg8eMUFtjDCP9ukpIwayfIZNrYFjv5
o46/KBjBQx2c8ECCNxgHQyU+4P97LsOo2YPcRLjMT8lvgBBt3OVE3agFE5sJN9T4
jusLLxei9m+BUH6L4b653Bo1hQJbtbdi5+FzZm7+8UBTO6tDyNy5+8Y/3TCrPDfX
kHRdQabCJiSrid8Xa7U9GzYRnKRqMWERlW4GHXv9eSQyjZeUw3OthAVvtkm4gV8S
3hgcXtbGD5WA6XEUaC8Vn4vcCaGZ8VuNGCjE6VU=
-----END CERTIFICATE-----`

	testKey = `-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDl7penVb8LHAoJ
1ShUG07CjfODx4GjT7Fhciul4o8P/bCPXAiHrn9pS0B6+Wmmxddhd6KOwHkcvan7
TCznx2Kd4+CeXPr12cDJsUp4p9/CtBmk8Aj3Hy+bD2fi/FJqqz/ujFUBF1oDsunk
qU4V+P0xbfMnIRaBQm7b6zVgeqD5v/buByMnArEaksbgI/cl8LOMS91FKTCZ1b3z
O5frTL3NPfsH28BLmA9vDZwgGthsDvONviyOHNxLyaMygf2Tz3iGIwVLZdQmHv5d
dhZ0XF1WiIJHqPUrE4RHXEoArYLtQrOj73HfVMbveaqv0bS0fcfQBQkl66WGRyea
MOcDW2WrAgMBAAECggEADEhzp4fIHeLQknfloKCY04HbyWVmAoBmXGtJ86GnvNXz
kaT7+6uRuOFBP5CFWVhmShmnAHM9xRBIvVjzaSpYlVCwFiWnbmzEhMHI1as6I9+V
Ix+DixgKZgErro+GI5akYqzyeY1yzJHJNuLoffoAJJzYCXYWCq+u1ma5Qj8qzEyg
AqrddSuSMpDLUqCD5CI5uRMZooqrsPS/28Ug2F3RvSowvYp3VJ3KY76qnzDFF+v8
FsGndmRUQRrN3KhZjz6dFZQG/8tzHb9L3UXfpQZCc7pkAXvee1Tq35Y0oTAHb5NU
eBrvi6Arjw3g0Poe/OFrIFjCPCMagrnLLoj9Eg+i4QKBgQD/Y05eLeroVvSjRkb1
PT967vgKa5Jn2SAqBmYdt6WAmkmENCcF2QTFFqptx8NIPVt2hWlohNu+VGZZp2GM
V6JN6ifb301AwOlkko2f03M4J2/p/G8zHV4NJ4PyJWVK3WtpSBib//8NfRlNOauB
bvgrTOOYaDifwCcorfBjBqi/8QKBgQDme6rwsocx+gVlO9ceq+tdFJowLgXELGK+
hGqsnqe+Qq5rlfnWhTZs0dbCkuJT89UM9JtitRsxT9xz66HaMdqDux92ESwt0IWy
nMmJ8LGcX6kadcqwMnXC14WNpUUaLxTIY6I2klu0+E3NM9UJl4j5d6isaojuGEq0
rhX8x+zbWwKBgQCp0CVW2B9feBpYyqz5+kzQeD9z5k1GQghyCSkzT1574ZtKjcb4
y3Gxfz25m1+NFEdRyqnpNpZKuyIHMRXa1JZ2SmFQgO2ERgGqvwvunxH437g5lIF4
MmnMQ18nzpfIrOvz6F18tT6pgGongFY6zUe0uv6G453rE0C2etnhbpcccQKBgBb0
VBb6wMoya10ks40DdEJl7eFEhCCAhykQSQt+FZi2TWa7nhFGXSBDWc8xD8dqrlpG
9j7DaLzlhkApRIpVkryx4zVACpVZgidCxDOvvBCl2lKfTptzuxS3oD52KkasT7aR
bbNfqjCA1kbMlbgJ1oN57luVlKOZ2b7a46e0RZunAoGAbZD0Mh/i5iWZvxszp59R
ACkUXcjeX+iUKCRQWiQE4oy0h/TLReKuqouAN1Gox42dFAaC/vtRzHkDM9GkoNOM
SK2B0oc1jX8rPwBYwRs3SCZe/5hubS15ROtmGhu6M+BYCJ0eQE5t/art4R5Hf5Wk
DAPOIqrArSSlC3ebv0TvI8w=
-----END PRIVATE KEY-----`

	testCert = `-----BEGIN CERTIFICATE-----
MIID5TCCAs2gAwIBAgIUFny3jx8px1X1MtSV/It9eULPZSQwDQYJKoZIhvcNAQEL
BQAwfDELMAkGA1UEBhMCVVMxEzARBgNVBAgMCk5ldyBTd2VkZW4xEzARBgNVBAcM
ClN0b2NraG9sbSAxCzAJBgNVBAoMAnV0MQswCQYDVQQLDAJ1dDEVMBMGA1UEAwwM
dW5pdC10ZXN0LWNhMRIwEAYJKoZIhvcNAQkBFgMuLi4wHhcNMjIwODA0MjE0ODUw
WhcNMzIwODAxMjE0ODUwWjCBgzELMAkGA1UEBhMCVVMxEzARBgNVBAgMCk5ldyBT
d2VkZW4xEzARBgNVBAcMClN0b2NraG9sbSAxCzAJBgNVBAoMAnV0MQswCQYDVQQL
DAJ1dDEcMBoGA1UEAwwTdXQtc2VydmVyLW9yLWNsaWVudDESMBAGCSqGSIb3DQEJ
ARYDLi4uMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA5e6Xp1W/CxwK
CdUoVBtOwo3zg8eBo0+xYXIrpeKPD/2wj1wIh65/aUtAevlppsXXYXeijsB5HL2p
+0ws58dinePgnlz69dnAybFKeKffwrQZpPAI9x8vmw9n4vxSaqs/7oxVARdaA7Lp
5KlOFfj9MW3zJyEWgUJu2+s1YHqg+b/27gcjJwKxGpLG4CP3JfCzjEvdRSkwmdW9
8zuX60y9zT37B9vAS5gPbw2cIBrYbA7zjb4sjhzcS8mjMoH9k894hiMFS2XUJh7+
XXYWdFxdVoiCR6j1KxOER1xKAK2C7UKzo+9x31TG73mqr9G0tH3H0AUJJeulhkcn
mjDnA1tlqwIDAQABo1cwVTBTBgNVHREETDBKhwR/AAABhwQYMwIkggl0dW5uZWxz
dmOCMWVjMi0zLTEwMS0xMTAtMTU3LnVzLXdlc3QtMS5jb21wdXRlLmFtYXpvbmF3
cy5jb20wDQYJKoZIhvcNAQELBQADggEBAAk128WfGC0Dbi4TrBOPrCNYuL56f1bB
NM5oNhJWtvxAhw186wy2KS8TVxOhz+eS70oAG/EEwGqfEh+yW66lnx4FDz7Dy9F0
ml+9J/X0NpcpNCGldoxOUJDVJre6XnzmGPxf3WBvT4OOV+E9mtBjXfeEsxb7zLsl
2LwDog7WqrRY0sgpE768YBrAnZJ/WvYZitqbJ5gKhtsoexRjl2YBH+P1FIjTzivp
rJKesfUZuF6Jwpgz54HIft1AsORYyFPsa94SJYXTdPbcMMYsmM/BhCupRF61ZOsi
NiznEMOH4RX5BX5TV+e43yXw56mv4vZXRSxqGylLyadH4XBXqVgVB+o=
-----END CERTIFICATE-----`
)

func (s *Suite) setup() error {
	l, err := net.Listen("tcp", "localhost:0")
	if err != nil {
		return err
	}
	s.l = l

	rtr := mux.NewRouter()
	rtr.HandleFunc("/v1/pki/ca/pem", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/plain; charset=utf-8")
		_, err := w.Write([]byte(testCACert))
		if err != nil {
			return
		}
		if s.forceErr {
			http.Error(w, "forced error", http.StatusInternalServerError)
		}
	})

	rtr.HandleFunc("/v1/pki/cloud-cert", func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "Bearer test-site-manager-token" {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		resp := &certs.CertificateResponse{
			Key:         testKey,
			Certificate: testCert,
		}
		c, err := json.Marshal(resp)
		if err != nil {
			panic(err)
		}
		w.Header().Set("Content-Type", "application/json")
		_, err = w.Write(c)
		if err != nil {
			return
		}
		if s.forceErr {
			http.Error(w, "forced error", http.StatusInternalServerError)
		}
	})

	s.srv = httptest.NewUnstartedServer(rtr)
	s.srv.Listener = l
	s.srv.StartTLS()
	s.tc = &http.Client{
		Timeout: 10 * time.Second,
		Transport: &http.Transport{
			// disable cert verification as this is a local server
			TLSClientConfig: &tls.Config{InsecureSkipVerify: true},
		},
	}
	return nil
}

// Teardown closes the connection
func (s *Suite) Teardown() {
	os.RemoveAll(s.certClientDir)
	s.srv.Close()
	s.cancel()
}

func (s *Suite) getSite(uuid string) (*types.SiteGetResponse, error) {
	r, err := s.tc.Get(s.MgrURL + "/v1/site/" + uuid)
	if err != nil {
		return nil, err
	}
	if http.StatusOK != r.StatusCode {
		return nil, fmt.Errorf("status code: %v", r.StatusCode)
	}
	defer r.Body.Close()
	c, err := io.ReadAll(r.Body)
	if err != nil {
		return nil, err
	}
	sgr := &types.SiteGetResponse{}
	err = json.Unmarshal(c, sgr)
	if err != nil {
		return nil, err
	}
	if sgr.OTP == "" {
		return nil, fmt.Errorf("OTP is empty")
	}
	return sgr, nil
}

func (s *Suite) getSiteCreds(uuid, otp string) (*types.SiteCredsResponse, error) {
	credsReq := &types.SiteCredsRequest{
		SiteUUID: uuid,
		OTP:      otp,
	}
	b := new(bytes.Buffer)
	err := json.NewEncoder(b).Encode(credsReq)
	if err != nil {
		return nil, err
	}
	r, err := s.tc.Post(s.MgrURL+"/v1/sitecreds", "application/json", b)
	if err != nil {
		return nil, err
	}
	if http.StatusOK != r.StatusCode {
		return nil, fmt.Errorf("status code: %v", r.StatusCode)
	}
	defer r.Body.Close()
	c, err := io.ReadAll(r.Body)
	if err != nil {
		return nil, err
	}
	scr := &types.SiteCredsResponse{}
	err = json.Unmarshal(c, scr)
	if err != nil {
		return nil, err
	}
	return scr, nil
}

func (s *Suite) getSiteCredsRaw(uuid, otp string) (*http.Response, error) {
	credsReq := &types.SiteCredsRequest{
		SiteUUID: uuid,
		OTP:      otp,
	}
	b := new(bytes.Buffer)
	err := json.NewEncoder(b).Encode(credsReq)
	if err != nil {
		return nil, err
	}
	r, err := s.tc.Post(s.MgrURL+"/v1/sitecreds", "application/json", b)
	if err != nil {
		return nil, err
	}
	return r, nil
}

func (s *Suite) createSite(uuid string) (*http.Response, error) {
	req := &types.SiteCreateRequest{
		SiteUUID: uuid,
		Name:     "ut-site",
		Provider: "ut",
	}
	b := new(bytes.Buffer)
	err := json.NewEncoder(b).Encode(req)
	if err != nil {
		return nil, err
	}
	r, err := s.tc.Post(s.MgrURL+"/v1/site", "application/json", b)
	if err != nil {
		return nil, err
	}

	return r, nil
}

// TestManagerCreateSite creates the site
func TestManagerCreateSite() (*Suite, error) {
	ts := &Suite{}
	err := ts.setup()
	if err != nil {
		return ts, err
	}

	ts.certClientDir, err = os.MkdirTemp("", "site-manager-cert-client-")
	if err != nil {
		return ts, err
	}
	caPath := filepath.Join(ts.certClientDir, "ca.crt")
	err = os.WriteFile(caPath, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: ts.srv.Certificate().Raw}), 0600)
	if err != nil {
		return ts, err
	}
	tokenPath := filepath.Join(ts.certClientDir, "token")
	err = os.WriteFile(tokenPath, []byte("test-site-manager-token"), 0600)
	if err != nil {
		return ts, err
	}
	fcrd := fakecrdclient.NewSimpleClientset()
	o := Options{
		credsMgrCAFile:    caPath,
		credsMgrTokenFile: tokenPath,
		credsMgrURL:       fmt.Sprintf("https://%s", ts.l.Addr().String()),
		ingressHost:       "test-host",
		listenPort:        "0",
		namespace:         "csm",
	}

	ctx := core.NewDefaultContext(context.Background())
	runCtx, cancel := context.WithCancel(ctx)
	ts.cancel = cancel
	m, err := newSiteManager(runCtx, o, fcrd)
	if err != nil {
		return ts, err
	}
	m.Start(runCtx)
	log := m.log

	testURL := fmt.Sprintf("https://%s", m.l.Addr().String())
	log.Infof("mgr serving at %v", testURL)
	ts.MgrURL = testURL

	testCase("Create site")
	r, err := ts.createSite(Testuuid1)
	if err != nil {
		log.Infof("%s", err.Error())
		return ts, err
	}
	if http.StatusOK != r.StatusCode {
		return ts, fmt.Errorf("status code is !OK %+v", r.StatusCode)
	}

	testCase("Get site")
	sgr, err := ts.getSite(Testuuid1)
	if err != nil {
		log.Infof("%s", err.Error())
		return ts, err
	}
	log.Infof("Got site: %+v", sgr)
	// save for roll test
	ts.UUID1OTP = sgr.OTP

	return ts, nil
}

// TestManagerSiteTest run tests on a site
func (s *Suite) TestManagerSiteTest() error {
	ts := s
	tc := ts.tc
	testURL := fmt.Sprintf("https://%s", s.l.Addr().String())
	testCase("Get site credentials - bad OTP")
	httpResp, err := ts.getSiteCredsRaw(Testuuid1, "guessanOTP12345")
	if err != nil {
		return err
	}
	defer httpResp.Body.Close()
	if http.StatusOK == httpResp.StatusCode {
		return fmt.Errorf("status code is OK %+v", httpResp.StatusCode)
	}

	testCase("Get site credentials")
	scr, err := ts.getSiteCreds(Testuuid1, ts.UUID1OTP)
	if err != nil {
		return err
	}

	fmt.Printf("Got site creds: %+v", scr)

	testCase("Get site credentials again")
	httpResp, err = ts.getSiteCredsRaw(Testuuid1, ts.UUID1OTP)
	if err != nil {
		return err
	}
	defer httpResp.Body.Close()
	if http.StatusOK != httpResp.StatusCode {
		return fmt.Errorf("status code is !OK %+v", httpResp.StatusCode)
	}

	testCase("Register site")
	r, err := tc.Post(testURL+"/v1/site/register/"+Testuuid1, "", nil)
	if err != nil {
		return err
	}
	defer r.Body.Close()
	if http.StatusOK != r.StatusCode {
		return fmt.Errorf("status code is !OK %+v", r.StatusCode)
	}

	testCase("Check site bootstrap status after registration")
	sgr, err := ts.getSite(Testuuid1)
	if err != nil {
		return err
	}
	if crdsv1.SiteRegistrationComplete != sgr.BootstrapState {
		return fmt.Errorf("SiteRegistration !Complete %+v", sgr.BootstrapState)
	}

	testCase("Get site credentials for a registered site")
	httpResp, err = ts.getSiteCredsRaw(Testuuid1, sgr.OTP)
	if err != nil {
		return err
	}
	defer httpResp.Body.Close()
	if http.StatusOK != httpResp.StatusCode {
		return fmt.Errorf("status code is !OK %+v", httpResp.StatusCode)
	}

	testCase("Create duplicate site")
	r, err = ts.createSite(Testuuid1)
	if err != nil {
		return err
	}
	defer r.Body.Close()
	if http.StatusOK != r.StatusCode {
		return fmt.Errorf("status code is !OK %+v", r.StatusCode)
	}

	testCase("Register non-existent site")
	r, err = tc.Post(testURL+"/v1/site/register/"+testuuid2, "", nil)
	if err != nil {
		return err
	}
	defer r.Body.Close()
	if http.StatusOK != r.StatusCode {
		return fmt.Errorf("status code is !OK %+v", r.StatusCode)
	}

	testCase("Expired OTP")
	otpDuration = 100 * time.Millisecond
	r, err = ts.createSite(testuuid2)
	if err != nil {
		return err
	}
	defer r.Body.Close()
	if http.StatusOK != r.StatusCode {
		return fmt.Errorf("status code is !OK %+v", r.StatusCode)
	}

	sgr, err = ts.getSite(testuuid2)
	if err != nil {
		return err
	}
	if sgr.OTP == "" {
		return fmt.Errorf("OTP is empty")
	}
	time.Sleep(110 * time.Millisecond)

	httpResp, err = ts.getSiteCredsRaw(testuuid2, sgr.OTP)
	if err != nil {
		return err
	}
	defer httpResp.Body.Close()
	if http.StatusOK != httpResp.StatusCode {
		return fmt.Errorf("status code is !OK %+v", httpResp.StatusCode)
	}
	otpDuration = 24 * time.Hour

	testCase("Delete a site")
	dreq, err := http.NewRequest(http.MethodDelete, testURL+"/v1/site/"+testuuid2, nil)
	if err != nil {
		return err
	}
	httpResp, err = tc.Do(dreq)
	if err != nil {
		return err
	}
	defer httpResp.Body.Close()
	if http.StatusOK != httpResp.StatusCode {
		return fmt.Errorf("status code is !OK %+v", httpResp.StatusCode)
	}

	testCase("Delete non-existent site")
	dreq, err = http.NewRequest(http.MethodDelete, testURL+"/v1/site/"+testuuid2, nil)
	if err != nil {
		return err
	}
	httpResp, err = tc.Do(dreq)
	if err != nil {
		return err
	}
	defer httpResp.Body.Close()
	if http.StatusOK != httpResp.StatusCode {
		return fmt.Errorf("status code is !OK %+v", httpResp.StatusCode)
	}

	testCase("Get a non-existent site")
	httpResp, err = tc.Get(testURL + "/v1/site/" + testuuid2)
	if err != nil {
		return err
	}
	defer httpResp.Body.Close()
	if http.StatusOK != httpResp.StatusCode {
		return fmt.Errorf("status code is !OK %+v", httpResp.StatusCode)
	}

	testCase("Roll a site")
	r, err = tc.Post(testURL+"/v1/site/roll/"+Testuuid1, "", nil)
	if err != nil {
		return err
	}
	defer r.Body.Close()
	if http.StatusOK != r.StatusCode {
		return fmt.Errorf("status code is !OK %+v", r.StatusCode)
	}

	sgr, err = ts.getSite(Testuuid1)
	if err != nil {
		return err
	}
	if crdsv1.SiteAwaitHandshake != sgr.BootstrapState {
		return fmt.Errorf("!SiteAwaitHandshake %+v", sgr.BootstrapState)
	}
	if sgr.OTP != s.UUID1OTP {
		return fmt.Errorf("!SiteAwaitHandshake %+v", sgr.BootstrapState)
	}

	_, err = ts.getSiteCreds(Testuuid1, sgr.OTP)
	if err != nil {
		return err
	}
	testCase("Roll a non-existent site")
	r, err = tc.Post(testURL+"/v1/site/roll/"+testuuid2, "", nil)
	if err != nil {
		return err
	}
	defer r.Body.Close()
	if http.StatusOK != r.StatusCode {
		return fmt.Errorf("status code is !OK %+v", r.StatusCode)
	}

	return nil
}

func testCase(s string) {
	fmt.Printf("\n=====\n===== Test Case: %s\n=====\n", s)
}
