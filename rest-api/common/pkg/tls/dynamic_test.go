// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tls

import (
	"bytes"
	"crypto/tls"
	"crypto/x509"
	"encoding/pem"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/sirupsen/logrus"
	logtest "github.com/sirupsen/logrus/hooks/test"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

const (
	testRefreshPeriod = 30 * time.Millisecond
)

var (
	test1Key = `-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC2lKoA7JyBuyTh
mrQxGPv51tjuVD94lRjaAJaIm/vTm4vfoAsU/arpgj9zMknochDadA+tkk6yMstN
jnXjDlwMUYIqoe61g9tTXxCDnNT5a3d37S8dGj44zLVr+uhVzAZ05x3usqdTZZ3l
hAaPyXE65PrhY3BidWHa01K73cVN1fLPY8/eyIsUrPSmq1rSg9AhaRbzdE5kMW13
AxZoOMH8tsFl48QQkfgSJ8Z/4LD1RSAGl6Lz7mn0WA1/TLO1vBCsLfHj0nylJK3Q
cNCeend+YAT5y1mUP/FPIBBSapAcAQCwJbg/X3Sw6PnD09FLLwrDC4Nopu7kM6Or
1UbgkM0VAgMBAAECggEAPwgJvLHywfK6o8wFwyFt8+2RDI43L0jBwJkNXvICuSXs
3vHggYmlVGHrx7gnvcCLQu9objKhSnGwsACrgAx4CKSm/FLVFwMDV7/s8pLVD5pj
LxrJ9hEWRAOf6jw/s0bxP7B+K+avT2I4ZYDzxvXzSjK8zczHgqYldycXW9YPBHRv
CuTzWdIWdbnuolJA6QGCOOTNswiNjWsMv2Fd8wlvorBSm5/usadNrL/ngeoqh7Qz
fY/TlK4zzy10pJ9nT/ZqoiMiMYSgrVzSK7Woh1RQSBkyNkJw3o4E2cuGMYH7v2Bb
GeU7gB6aSoimY//Lns62RMx1E93hqDo/1qYbzvb8gQKBgQDvJWTPYGrkjpGZzc0I
LvEqxwqbFByOI3cG6LotV1HkFoIb596unJLRPCvC1ruIZEfnPWPT3BAdFL6Vik5O
A3Y2Iv6bJi4Y9vbhiXpFfEI/+nkZrEoWw4vn3l2r4oulrVkoB4fNsqXyE9q6fawx
5FlOZr5CjuDNWS644IVgEmI+9QKBgQDDcrvvhhmq01xVFcFan9WMZiivoPMQcSYv
RCjUMl3y3UdV2N9eGX0bC43TpGtRNgUvcUgpsoXp9yd2JU7TDQhrEP07jhOFtFcD
HVZgzgEj5t5CfYeERafP6uNozd+DNqguLIig3OxyAGyGHua2mg3EMi3vXkiNWa/u
GWtqDr9BoQKBgAFakdaGsjQ3BmX7f0Sjl2PpmorEM2EunDbizGMDUohbBEOKLX2J
j1812v2QX6FnB+0sMMt7PHAdtPJ9xPG2HU4zJoPUVIB5rW4bbCDGkk1wao0Vp5m3
Y6xdWuRlNOssLwwF9uPYNg5HxH43xejGZScHd95Cls0yywvq4XZoxDudAoGAFOQF
nIOL6MtwuhN6OFKPQ9ODk8ozUNWXTEQPzSaZDiWCw3VL4sX8rlBc13tikSqiAUEt
gm93ituFF0bDlyF0feUx/BSil48AIfAX1H8QdiLuLNM4EfZUCpBDwGcI9gB4l37h
F7ileUX8U5Wn+WqcABWQ/V3piVpFyMBkz9BFtyECgYEAsq9sFZK6M79iozmSMo6w
OParZ/5FyS+tOhtWtxBP24tdGq8JiU+qHAY2PdTx8pcJNBRGaZCXbKedFR9GqsNO
JPaElnYfcvFvxZRgMJU9hIF4dYGxpF38lNK5gs/+9iNBQnsYiwF0Do2uyMTrZOOx
mzC+3hEbQAknvGR9WLSQKRs=
-----END PRIVATE KEY-----`

	test1Cert = `-----BEGIN CERTIFICATE-----
MIID5jCCAs6gAwIBAgIUXaHasW/edogm9CSw629FmYe15h4wDQYJKoZIhvcNAQEL
BQAwfTELMAkGA1UEBhMCVVMxEzARBgNVBAgMCk5ldyBTd2VkZW4xEzARBgNVBAcM
ClN0b2NraG9sbSAxCzAJBgNVBAoMAnV0MQswCQYDVQQLDAJ1dDEWMBQGA1UEAwwN
dW5pdC10ZXN0LWNhMjESMBAGCSqGSIb3DQEJARYDLi4uMB4XDTIyMDgwNDIzMzAx
M1oXDTMyMDgwMTIzMzAxM1owgYMxCzAJBgNVBAYTAlVTMRMwEQYDVQQIDApOZXcg
U3dlZGVuMRMwEQYDVQQHDApTdG9ja2hvbG0gMQswCQYDVQQKDAJ1dDELMAkGA1UE
CwwCdXQxHDAaBgNVBAMME3V0LXNlcnZlci1vci1jbGllbnQxEjAQBgkqhkiG9w0B
CQEWAy4uLjCCASIwDQYJKoZIhvcNAQEBBQADggEPADCCAQoCggEBALaUqgDsnIG7
JOGatDEY+/nW2O5UP3iVGNoAloib+9Obi9+gCxT9qumCP3MySehyENp0D62STrIy
y02OdeMOXAxRgiqh7rWD21NfEIOc1Plrd3ftLx0aPjjMtWv66FXMBnTnHe6yp1Nl
neWEBo/JcTrk+uFjcGJ1YdrTUrvdxU3V8s9jz97IixSs9KarWtKD0CFpFvN0TmQx
bXcDFmg4wfy2wWXjxBCR+BInxn/gsPVFIAaXovPuafRYDX9Ms7W8EKwt8ePSfKUk
rdBw0J56d35gBPnLWZQ/8U8gEFJqkBwBALAluD9fdLDo+cPT0UsvCsMLg2im7uQz
o6vVRuCQzRUCAwEAAaNXMFUwUwYDVR0RBEwwSocEfwAAAYcEGDMCJIIJdHVubmVs
c3ZjgjFlYzItMy0xMDEtMTEwLTE1Ny51cy13ZXN0LTEuY29tcHV0ZS5hbWF6b25h
d3MuY29tMA0GCSqGSIb3DQEBCwUAA4IBAQBJhnDMdsoSkmDxSVEqcDpmoCjHrult
dST5uGZOaG5vR7T9bf6/3h9bVlcHMPLaWTEKfRn/t0hjcuyeDp4e1Ll4IXCh0wE9
uaYvgFAaDeC/1KQz1lFU4qgqWL5Tlfb57ScXTpp/JUKqiKaHm7YRkivVwFl6vQ30
/dp1qjtaNdXBEvCXo3xD7muJP51vvjjfgqpcRIztM2GoEN9Vwm/Br6ZUVFmVIw9R
0pzY2OreXbvFDQjhysF9P4XiPrfZBu+buVHBVzYA8WnkGNHKiOg3xWJURM17hr8q
ETqlkngFrct3/VD5QSevGwZ3i6WOGew1HjVcoBIWcDqvjRnB/YmrXAQm
-----END CERTIFICATE-----`

	test2Key = `-----BEGIN PRIVATE KEY-----
MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQDsL5nsFVUJtcvM
4Do46Ud0Kp3036Z+fMtnIKWx2o1LfRnRZr5HAH7iqT5skJ9CcrV5fLdHdd4LKwQD
LOh/X3OCAMVDf2np5IS1re89wNuQfdBwCDC4D0J207iubiQ7R0RwOTxVffkAyoJ+
HblRxK2sdOgLHgyQTzWLvcNCiCOZFfVwmyrAhUHYH0c7PAWswSlq3Qhyr3T4RDAL
QZjqGXYFhJePDbnwlIWfa0n1uCLCrg25S5Zg0uuCu0PT0M26xwa+/r7Jomv0gzM3
yieFSnXTm1uTJ8wNgnA/NWdF4QkDoCHhr1o/6UylrlJ2YnYLuFYeXXN7RpZyNz3O
xfv+cF7rAgMBAAECggEAQc9drzeec08xk0ujTXpKy6aYTsQGq1XwgzLImI8SMceQ
6xUazcPolxWbbDq79ZLq2AgVNZc1IJ2Cx7O6sMsS71VxocYd5+shw1HMyMM1KsSz
0JOnp5Gw7lU+L3RHKjFIc5CvLA8m076Zr7Ruj8cisVv90CM2UvuPKvncL2ypppy7
tZIBW13X/VW45Rs7JdPI7zF6PATdN5ccSGlh/oADmMGDunFdsCIquQRCKqY5ZbaI
UF8o81ww6FpoeVwR9zfmhqyDULZzuTP+j5z4Oiq81qNJFA2FveC492aZm7ggPuqk
4QIRYKtRpHL7VGiqKOI5eY2vabei4bSJua9v8upiqQKBgQD4sxv84ooT9Wy1E+T3
I/xfecYc6/VhazPhCPI+NGwbzokbwLJ7HybixDqej44qotkyUFILY5I6Ft8MgELU
lJCzHm6lZuG++99hjRJg2oxYYJgM9xwur4HvYkhBtCKRfPh5g7HradfvpSUgKaBG
xK8M7H15xpiiJU8/J0StUuSfDQKBgQDzHnSxqwT8zX2smIb75vobMiVMJuRXFoPQ
AQn4CC21Yhpje9qFhcxO4VEQkzq4oicQVTq9k4xAXWqRIpCTggKZr5OeOeQC5fFO
FyzO9tocXVLpHyRjmoBQ+PhnYkBlHldqhTrRB1IVm1o42vBTu6skxtaG8Od6yxzp
BRt6ko831wKBgFdbMn2FZVLVZjXEoyxcK42tzHTkPPDXIwXsiopnB4JM7cQdz5OH
wbTtkFmZuyomwXv20prFgtt8pSRS+SaKeLkx+1OF682V00UEtGvo2FtCsqX7Np7/
bviS4SaTC4FnEDA+ngQ+zWaT75J4jJ/O/l3fw8M+iuaJjGh2dp0a/MsRAoGAI564
tjc6Wde5rAoE7O9ggY+NS2T/W4se8ODWFxMLr2GaQC0rTRjXYE8+01De76JCWvBB
1PjDOcL2FCGeUR5hRyckV7BfqdUKz8gxdnlQZ4t81E8Nw9IlLrfrnSoWCTqy0BaJ
EYsjCatjQqVBRONgJdlEIS02nRUZPULUTdcfSK0CgYA6LBByzvXtrVnhqm6sfFbQ
I+5t5BgIhG3U1bB7/IHQ1I2RYTxafcceUaQUaxy6qVrp2JlIH6reKFv5aK7XfXnv
Ebbo8Mbszs4JsvOvBsZ22dlvD3p0KHg2PTuWhFi0XJtdP9RBfQjvjelDnOmTKpoh
AZVtI0W45jAU+ebkn3WW+w==
-----END PRIVATE KEY-----`

	test2Cert = `-----BEGIN CERTIFICATE-----
MIID5jCCAs6gAwIBAgIUXaHasW/edogm9CSw629FmYe15h8wDQYJKoZIhvcNAQEL
BQAwfTELMAkGA1UEBhMCVVMxEzARBgNVBAgMCk5ldyBTd2VkZW4xEzARBgNVBAcM
ClN0b2NraG9sbSAxCzAJBgNVBAoMAnV0MQswCQYDVQQLDAJ1dDEWMBQGA1UEAwwN
dW5pdC10ZXN0LWNhMjESMBAGCSqGSIb3DQEJARYDLi4uMB4XDTIyMDgwNDIzMzAx
M1oXDTMyMDgwMTIzMzAxM1owgYMxCzAJBgNVBAYTAlVTMRMwEQYDVQQIDApOZXcg
U3dlZGVuMRMwEQYDVQQHDApTdG9ja2hvbG0gMQswCQYDVQQKDAJ1dDELMAkGA1UE
CwwCdXQxHDAaBgNVBAMME3V0LXdzLXR1bm5lbC1jbGllbnQxEjAQBgkqhkiG9w0B
CQEWAy4uLjCCASIwDQYJKoZIhvcNAQEBBQADggEPADCCAQoCggEBAOwvmewVVQm1
y8zgOjjpR3QqnfTfpn58y2cgpbHajUt9GdFmvkcAfuKpPmyQn0JytXl8t0d13gsr
BAMs6H9fc4IAxUN/aenkhLWt7z3A25B90HAIMLgPQnbTuK5uJDtHRHA5PFV9+QDK
gn4duVHErax06AseDJBPNYu9w0KII5kV9XCbKsCFQdgfRzs8BazBKWrdCHKvdPhE
MAtBmOoZdgWEl48NufCUhZ9rSfW4IsKuDblLlmDS64K7Q9PQzbrHBr7+vsmia/SD
MzfKJ4VKddObW5MnzA2CcD81Z0XhCQOgIeGvWj/pTKWuUnZidgu4Vh5dc3tGlnI3
Pc7F+/5wXusCAwEAAaNXMFUwUwYDVR0RBEwwSocEfwAAAYcEGDMCJIIJdHVubmVs
c3ZjgjFlYzItMy0xMDEtMTEwLTE1Ny51cy13ZXN0LTEuY29tcHV0ZS5hbWF6b25h
d3MuY29tMA0GCSqGSIb3DQEBCwUAA4IBAQAy71dLzpbeVsXzhXKY/aGerRaoPjib
Rl3pvuaXBcd1ON/Yh+c3u7H+7Ckk0Sh2sfrsRn8trmWEqM2TSejAAQW4VoSw/f/Q
jlEz2J+WLr2HbpdETEFAGo7QfX9MdnkZSdccPIZaajMDAU401EtK9mMjXhWcWDS5
xv4bFu18W51nyM1dku0JZPHbQCpsPIiYWAoseATkYQWftNxOZzla6zgRhI80+M6I
A8tqIHXYM66YK3EjHouu1cBssTmQL+OcrIKQM0KBwE//4uTZQ8YOp5dw46HWVohw
pJrPnw3J8/Yu8Tx8SjwwckcKG56/otL8JXSAr7vUAzKo7nYNvw0WPAUi
-----END CERTIFICATE-----`

	testCaCert = `-----BEGIN CERTIFICATE-----
MIID2zCCAsOgAwIBAgIUA58RgmEmVEOW0E0XdduRHhp/w4kwDQYJKoZIhvcNAQEL
BQAwfTELMAkGA1UEBhMCVVMxEzARBgNVBAgMCk5ldyBTd2VkZW4xEzARBgNVBAcM
ClN0b2NraG9sbSAxCzAJBgNVBAoMAnV0MQswCQYDVQQLDAJ1dDEWMBQGA1UEAwwN
dW5pdC10ZXN0LWNhMjESMBAGCSqGSIb3DQEJARYDLi4uMB4XDTIyMDgwNDIzMzAx
M1oXDTMyMDgwMTIzMzAxM1owfTELMAkGA1UEBhMCVVMxEzARBgNVBAgMCk5ldyBT
d2VkZW4xEzARBgNVBAcMClN0b2NraG9sbSAxCzAJBgNVBAoMAnV0MQswCQYDVQQL
DAJ1dDEWMBQGA1UEAwwNdW5pdC10ZXN0LWNhMjESMBAGCSqGSIb3DQEJARYDLi4u
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA0KcoqEhevy3WZG0QhE8+
Y/p2IR2G4Ppot4LFuM+bAKogQ9ynBQcpTlch8GWZVht5zZdJYsZ5EX74+JluZlk/
jsDycJIY5OHjQ1kPudaqUmUP+oWpGRUpcoXs1Gk5hJe1T0BxAaSEGbu7DyLsrpHw
FW0T9fR1Ioj0Vo3kIznZHkm7GqNjP61OBVEtDTm00GRqQa/lOXPkWnqKs7SOHOtT
u93z2VBWgGKU2xrJxNySiAPebQBVIXV0ekJZf8rb2A7MED7wqb5ocoKz+uJJ9RKo
UcModLOPQ59F/sKxjfDU28D7E5p8vbo2mhHUY1FDhgP51ENFhTecA8lB50C4rZpC
PQIDAQABo1MwUTAdBgNVHQ4EFgQUcFp/ENqfM0f1b8wxdwFxk8Bun4UwHwYDVR0j
BBgwFoAUcFp/ENqfM0f1b8wxdwFxk8Bun4UwDwYDVR0TAQH/BAUwAwEB/zANBgkq
hkiG9w0BAQsFAAOCAQEAgTO1nehOell5Edo2vJzhnPZGfLB44l18PxAoy6oXwtUq
8tT1eWu9b6PzoqaOGqA3fknf/dWv/Q3+TPqPhzR5kDiENBKK/I0BfDBGlmNjcSLw
/cDms5/RToPl70Qg+Aip6jeKNO4pEjco7QZNMGaBSFFH85m0wkqJCbpk0Uvs0MUb
B4aNKRpQYAn/lixIkAay/mPl6sgMLRxSUT4YvjNPPHNyWZ1R6VO3SK2TPdRpxFSK
YBbgOHLmXaUdU4VrcsN80i1N1RdfRymZeX+sR+uKoamlOrvCW56dRjTUp5SmdRbV
nzDxC7jQdPaQQ0M8fH43fS0E6D9wH/hVJJFKJBJqew==
-----END CERTIFICATE-----`
)

func TestDynTLSCfg(t *testing.T) {
	refreshPeriod = testRefreshPeriod
	dir, err := os.MkdirTemp("", "certs")
	require.NoError(t, err)
	defer os.RemoveAll(dir)

	key1 := filepath.Join(dir, "t1.key")
	err = os.WriteFile(key1, []byte(test1Key), 0666)
	require.NoError(t, err)

	cert1 := filepath.Join(dir, "t1.pem")
	err = os.WriteFile(cert1, []byte(test1Cert), 0666)
	require.NoError(t, err)

	ca := filepath.Join(dir, "ca.pem")
	err = os.WriteFile(ca, []byte(testCaCert), 0666)
	require.NoError(t, err)

	c, err := NewDynTLSCfg(key1, cert1, ca)
	require.NoError(t, err)

	testCfg := &tls.Config{ServerName: "testServer"}
	c = c.WithTLSCfg(testCfg)

	cfg := c.ClientCfg()
	assert.Equal(t, cfg.ServerName, "testServer")
	assert.Equal(t, cfg.Certificates, []tls.Certificate(nil))
	// verify ca
	caCertPool := x509.NewCertPool()
	caCertPool.AppendCertsFromPEM([]byte(testCaCert))
	assert.True(t, caCertPool.Equal(cfg.RootCAs))

	pair1, err := tls.LoadX509KeyPair(cert1, key1)
	require.NoError(t, err)

	pair2, err := cfg.GetClientCertificate(nil)
	require.NoError(t, err)
	assert.True(t, reflect.DeepEqual(&pair1, pair2))

	// update creds
	err = os.WriteFile(key1, []byte(test2Key), 0666)
	require.NoError(t, err)

	err = os.WriteFile(cert1, []byte(test2Cert), 0666)
	require.NoError(t, err)
	time.Sleep(100 * time.Millisecond)

	// get certs again
	pair3, err := cfg.GetClientCertificate(nil)
	require.NoError(t, err)
	pair4, err := tls.LoadX509KeyPair(cert1, key1)
	require.NoError(t, err)
	assert.True(t, reflect.DeepEqual(pair3, &pair4))
	assert.False(t, reflect.DeepEqual(pair2, &pair4))
	c.Close()

	s, err := NewDynTLSCfg(key1, cert1, ca)
	require.NoError(t, err)
	defer s.Close()

	cfg = s.ServerCfg()
	sCfg, err := cfg.GetConfigForClient(nil)
	require.NoError(t, err)
	assert.True(t, reflect.DeepEqual(&sCfg.Certificates[0], &pair4))

	// update creds
	err = os.WriteFile(key1, []byte(test1Key), 0666)
	require.NoError(t, err)
	err = os.WriteFile(cert1, []byte(test1Cert), 0666)
	require.NoError(t, err)
	err = os.WriteFile(ca, []byte(test2Cert), 0666)
	require.NoError(t, err)
	time.Sleep(100 * time.Millisecond)

	// get certs again
	sCfg, err = cfg.GetConfigForClient(nil)
	require.NoError(t, err)
	assert.True(t, reflect.DeepEqual(&sCfg.Certificates[0], &pair1))

	// verify ca
	caCertPool = x509.NewCertPool()
	caCertPool.AppendCertsFromPEM([]byte(test2Cert))
	assert.True(t, caCertPool.Equal(sCfg.RootCAs))
	assert.True(t, caCertPool.Equal(sCfg.ClientCAs))
}

func TestNewDynTLSCfg(t *testing.T) {
	// This test changes global logging state and must not run in parallel.
	var globalLogs bytes.Buffer
	standard := logrus.StandardLogger()
	originalOutput, originalLevel := standard.Out, standard.GetLevel()
	standard.SetOutput(&globalLogs)
	standard.SetLevel(logrus.ErrorLevel)
	t.Cleanup(func() {
		standard.SetOutput(originalOutput)
		standard.SetLevel(originalLevel)
	})
	dir := t.TempDir()
	key := filepath.Join(dir, "tls.key")
	require.NoError(t, os.WriteFile(key, []byte(test1Key), 0600))
	cert := filepath.Join(dir, "tls.crt")
	require.NoError(t, os.WriteFile(cert, []byte(test1Cert), 0644))
	ca := filepath.Join(dir, "ca.crt")
	for name, tc := range map[string]struct {
		bundle        string
		wantErr       bool
		mismatchedKey bool
	}{
		"invalid startup identity": {bundle: testCaCert, wantErr: true, mismatchedKey: true},
		"no certificates":          {bundle: "not a certificate", wantErr: true},
		"partially valid bundle":   {bundle: testCaCert + "\n" + string(pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: []byte("invalid DER")}))},
	} {
		t.Run(name, func(t *testing.T) {
			keyPEM := test1Key
			if tc.mismatchedKey {
				keyPEM = test2Key
			}
			require.NoError(t, os.WriteFile(key, []byte(keyPEM), 0600))
			require.NoError(t, os.WriteFile(ca, []byte(tc.bundle), 0644))
			d, err := NewDynTLSCfg(key, cert, ca)
			if d != nil {
				d.Close()
			}
			if tc.wantErr {
				require.Error(t, err)
				assert.Nil(t, d)
				return
			}
			require.NoError(t, err)
			expected := x509.NewCertPool()
			require.True(t, expected.AppendCertsFromPEM([]byte(tc.bundle)))
			assert.True(t, expected.Equal(d.caCertPool))
			assert.Empty(t, globalLogs.String(), "startup CA diagnostics must not use the global logger")
		})
	}
}

func TestParseCABundle(t *testing.T) {
	invalidDER := string(pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: []byte("invalid DER")}))
	invalidPEM := "-----BEGIN CERTIFICATE-----\nnot-base64!\n-----END CERTIFICATE-----\n"
	auxiliary := string(pem.EncodeToMemory(&pem.Block{Type: "COMMENT", Bytes: []byte("auxiliary data")}))
	cases := map[string]struct {
		bundle  string
		wantErr bool
		wantLog bool
	}{
		"multiple certificates and auxiliary block": {bundle: testCaCert + "\n" + auxiliary + test2Cert},
		"CRLF certificate":                          {bundle: strings.ReplaceAll(testCaCert, "\n", "\r\n")},
		"marker in explanatory text":                {bundle: "# Example boundary: -----BEGIN CERTIFICATE-----\n" + testCaCert},
		"invalid DER alongside valid certificate":   {bundle: testCaCert + "\n" + invalidDER, wantLog: true},
		"malformed PEM before valid certificate":    {bundle: invalidPEM + testCaCert, wantLog: true},
		"malformed PEM after valid certificate":     {bundle: testCaCert + "\n" + invalidPEM, wantLog: true},
		"malformed PEM before auxiliary block":      {bundle: testCaCert + "\n" + invalidPEM + auxiliary, wantLog: true},
		"no certificate blocks":                     {bundle: auxiliary, wantErr: true},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			var logs bytes.Buffer
			logger := logrus.New()
			logger.SetOutput(&logs)
			pool, err := parseCABundle([]byte(tc.bundle), logger)
			if tc.wantErr {
				require.Error(t, err)
				assert.Nil(t, pool)
				return
			}
			require.NoError(t, err)
			expected := x509.NewCertPool()
			// Match the predecessor's standard-library loading semantics exactly.
			require.True(t, expected.AppendCertsFromPEM([]byte(tc.bundle)))
			assert.True(t, expected.Equal(pool))
			if tc.wantLog {
				assert.Contains(t, logs.String(), "level=error")
				assert.Contains(t, logs.String(), "using parseable certificates")
			} else {
				assert.Empty(t, logs.String())
			}
		})
	}
}

func TestDynTLSCfg_Refresh(t *testing.T) {
	cases := map[string]struct{ client, missingCA bool }{
		"server retains pool on invalid CA": {},
		"server retains pool on missing CA": {missingCA: true},
		"client retains pool on missing CA": {client: true, missingCA: true},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			dir := t.TempDir()
			key, cert, ca := filepath.Join(dir, "tls.key"), filepath.Join(dir, "tls.crt"), filepath.Join(dir, "ca.crt")
			require.NoError(t, os.WriteFile(key, []byte(test1Key), 0600))
			require.NoError(t, os.WriteFile(cert, []byte(test1Cert), 0644))
			require.NoError(t, os.WriteFile(ca, []byte(testCaCert), 0644))
			d, err := NewDynTLSCfg(key, cert, ca)
			require.NoError(t, err)
			defer d.Close()
			d.ticker.Stop() // Drive refresh explicitly so intermediate file writes cannot race the poller.
			var current func() (*tls.Certificate, *x509.CertPool, error)
			if tc.client {
				cfg := d.ClientCfg()
				current = func() (*tls.Certificate, *x509.CertPool, error) {
					cert, err := cfg.GetClientCertificate(nil)
					return cert, cfg.RootCAs, err
				}
			} else {
				cfg := d.ServerCfg()
				current = func() (*tls.Certificate, *x509.CertPool, error) {
					cfg, err := cfg.GetConfigForClient(nil)
					if err != nil {
						return nil, nil, err
					}
					return &cfg.Certificates[0], cfg.ClientCAs, nil
				}
			}
			originalCert, originalPool, err := current()
			require.NoError(t, err)
			invalid := string(pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: []byte("invalid DER")}))
			if tc.missingCA {
				require.NoError(t, os.Remove(ca))
			} else {
				require.NoError(t, os.WriteFile(ca, []byte(invalid), 0644))
			}
			d.refresh()
			d.Lock()
			assert.Same(t, originalPool, d.caCertPool)
			assert.Equal(t, []byte(testCaCert), d.cachedCa)
			assert.False(t, d.cacheUpdated)
			d.Unlock()
			unchangedCert, unchangedPool, err := current()
			require.NoError(t, err, "CA failure must not reject handshake callbacks")
			assert.Equal(t, originalCert, unchangedCert)
			assert.Same(t, originalPool, unchangedPool)

			// A missing or mismatched key must retain the last usable identity.
			require.NoError(t, os.Remove(key))
			d.refresh()
			retainedCert, _, err := current()
			require.NoError(t, err)
			assert.Equal(t, originalCert, retainedCert)
			// Renew identity without repairing the CA; refresh must still proceed.
			require.NoError(t, os.WriteFile(key, []byte(test2Key), 0600))
			d.refresh()
			retainedCert, _, err = current()
			require.NoError(t, err)
			assert.Equal(t, originalCert, retainedCert)
			require.NoError(t, os.WriteFile(cert, []byte(test2Cert), 0644))
			d.refresh()
			renewedCert, retainedPool, err := current()
			require.NoError(t, err)
			expectedCert, err := tls.LoadX509KeyPair(cert, key)
			require.NoError(t, err)
			assert.Equal(t, expectedCert, *renewedCert)
			assert.NotEqual(t, originalCert.Certificate, renewedCert.Certificate)
			assert.Same(t, originalPool, retainedPool)

			hook := logtest.NewLocal(d.logger)
			// A mixed bundle must install its usable certificates, as before strict validation.
			require.NoError(t, os.WriteFile(ca, []byte(test2Cert+"\n"+invalid), 0644))
			d.refresh()
			_, updatedPool, err := current()
			require.NoError(t, err)
			if tc.client {
				assert.Same(t, originalPool, updatedPool, "client CA hot reload remains out of scope")
				d.refresh()
				assert.Len(t, hook.AllEntries(), 1, "unchanged client CA must not repeat warnings")
				require.NoError(t, os.WriteFile(ca, []byte(testCaCert), 0644))
				d.refresh()
				require.NoError(t, os.WriteFile(ca, []byte(test2Cert), 0644))
				d.refresh()
				assert.Len(t, hook.AllEntries(), 3, "each newly observed CA change is reported")
				assert.Equal(t, []byte(testCaCert), d.cachedCa, "installed CA is not overwritten by observations")
			} else {
				expected := x509.NewCertPool()
				require.True(t, expected.AppendCertsFromPEM([]byte(test2Cert)))
				assert.True(t, expected.Equal(updatedPool))
				assert.NotSame(t, originalPool, updatedPool)
			}
		})
	}
}

func TestDynTLSCfg_CloseIsIdempotent(t *testing.T) {
	dir := t.TempDir()
	key := filepath.Join(dir, "tls.key")
	require.NoError(t, os.WriteFile(key, []byte(test1Key), 0600))
	cert := filepath.Join(dir, "tls.crt")
	require.NoError(t, os.WriteFile(cert, []byte(test1Cert), 0644))
	ca := filepath.Join(dir, "ca.crt")
	require.NoError(t, os.WriteFile(ca, []byte(testCaCert), 0644))

	dynamicConfig, err := NewDynTLSCfg(key, cert, ca)
	require.NoError(t, err)
	dynamicConfig.Close()
	assert.NotPanics(t, dynamicConfig.Close)
}
