/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */
#![cfg(test)]

// The certs here are taken from existing certs we've seen, with the actual strings changed to be
// generic. We're not testing the validation logic so it's ok if the signatures don't match. The
// text representation is here to make the tests easier to understand, only the lines between BEGIN
// CERTIFICATE and END CERTIFICATE are important. Because we are not actually validating these
// certs, it doesn't matter if they expire

pub(super) static CLIENT_CERT_DHCP: &str = r#"
Certificate:
    Data:
        Version: 3 (0x2)
        Serial Number:
            4c:27:bb:a5:93:c9:a3:fd:ee:cc:4d:dc:87:89:cd:da:88:09:86:23
        Signature Algorithm: sha256WithRSAEncryption
        Issuer: CN = site-root
        Validity
            Not Before: Oct 14 20:34:59 2024 GMT
            Not After : Nov 13 20:35:29 2024 GMT
        Subject:
        Subject Public Key Info:
            Public Key Algorithm: id-ecPublicKey
                Public-Key: (384 bit)
                pub:
                    04:87:d8:ed:b1:bc:56:21:ce:cc:58:75:e4:3d:72:
                    23:4d:6d:a3:f7:ee:2f:fe:c5:64:5a:bd:7a:e4:11:
                    b0:62:af:d9:f7:4a:5b:9c:c8:ac:44:0f:ea:fa:38:
                    24:47:4b:e7:4a:a0:09:5f:53:2c:af:80:28:5f:ac:
                    db:5d:9a:d2:33:9d:7e:1b:ed:f1:b3:70:fc:79:2a:
                    81:f9:90:74:a6:96:91:7b:6f:b1:39:15:c5:3a:65:
                    7b:9e:bf:f2:fd:91:fa
                ASN1 OID: secp384r1
                NIST CURVE: P-384
        X509v3 extensions:
            X509v3 Key Usage: critical
                Digital Signature, Key Encipherment, Key Agreement
            X509v3 Extended Key Usage:
                TLS Web Server Authentication, TLS Web Client Authentication
            X509v3 Subject Key Identifier:
                0F:B8:FF:B4:9E:53:D1:33:5F:B2:FC:61:37:01:AC:33:18:A5:28:8C
            X509v3 Authority Key Identifier:
                F6:BF:5F:8F:F7:08:E8:C5:AE:E9:74:60:1D:DD:AB:D3:28:31:50:48
            X509v3 Subject Alternative Name: critical
                DNS:carbide-dhcp.carbide-system.svc.cluster.local, URI:spiffe://example.test/carbide-system/sa/carbide-dhcp
    Signature Algorithm: sha256WithRSAEncryption
    Signature Value:
        8b:b5:cb:18:27:17:18:7a:af:12:f8:a5:66:32:2f:7f:10:0b:
        73:e4:56:90:de:1a:5a:a2:e4:32:4a:91:21:4d:cb:2d:be:53:
        71:01:8b:fa:96:75:d4:37:86:a2:75:d1:fe:6d:a9:12:77:c0:
        02:dc:e1:cc:8c:55:aa:17:3b:82:85:2c:54:f7:fc:c5:b2:90:
        e3:3e:4b:3b:29:47:64:9d:8a:32:4e:7a:42:90:9f:94:05:59:
        61:fe:08:e2:f1:f0:05:36:32:b0:82:a2:11:35:5f:ca:6c:ce:
        4a:cc:7d:37:f2:d9:70:b2:d1:c5:cb:7a:82:5d:4a:71:1e:65:
        af:d6:06:15:1b:05:53:d6:3a:dc:d4:a6:7c:cb:75:a7:7f:ef:
        9e:fa:b4:c2:ab:b3:90:c7:a1:64:f7:83:d9:6d:7d:ae:65:e0:
        4c:60:9e:d0:4c:f6:0b:db:fe:27:8d:a9:8d:9b:84:9e:02:b5:
        22:1f:81:18:02:cf:71:18:87:6e:5d:a2:9d:66:d0:ef:62:a4:
        15:9c:18:94:fd:56:cc:7c:bc:ff:a3:48:b9:75:6d:78:47:cd:
        66:f2:7b:cc:c0:ed:e8:4c:97:c1:de:17:1b:ef:e0:9c:da:18:
        2e:bc:b5:6e:19:1f:ab:b9:52:66:13:40:95:3c:8d:23:9c:af:
        26:77:4c:86
-----BEGIN CERTIFICATE-----
MIIC2jCCAcKgAwIBAgIUTCe7pZPJo/3uzE3ch4nN2ogJhiMwDQYJKoZIhvcNAQEL
BQAwFDESMBAGA1UEAxMJc2l0ZS1yb290MB4XDTI0MTAxNDIwMzQ1OVoXDTI0MTEx
MzIwMzUyOVowADB2MBAGByqGSM49AgEGBSuBBAAiA2IABIfY7bG8ViHOzFh15D1y
I01to/fuL/7FZFq9euQRsGKv2fdKW5zIrEQP6vo4JEdL50qgCV9TLK+AKF+s212a
0jOdfhvt8bNw/HkqgfmQdKaWkXtvsTkVxTple56/8v2R+qOB5TCB4jAOBgNVHQ8B
Af8EBAMCA6gwHQYDVR0lBBYwFAYIKwYBBQUHAwEGCCsGAQUFBwMCMB0GA1UdDgQW
BBQPuP+0nlPRM1+y/GE3AawzGKUojDAfBgNVHSMEGDAWgBT2v1+P9wjoxa7pdGAd
3avTKDFQSDBxBgNVHREBAf8EZzBlgi1jYXJiaWRlLWRoY3AuY2FyYmlkZS1zeXN0
ZW0uc3ZjLmNsdXN0ZXIubG9jYWyGNHNwaWZmZTovL2V4YW1wbGUudGVzdC9jYXJi
aWRlLXN5c3RlbS9zYS9jYXJiaWRlLWRoY3AwDQYJKoZIhvcNAQELBQADggEBAIu1
yxgnFxh6rxL4pWYyL38QC3PkVpDeGlqi5DJKkSFNyy2+U3EBi/qWddQ3hqJ10f5t
qRJ3wALc4cyMVaoXO4KFLFT3/MWykOM+SzspR2SdijJOekKQn5QFWWH+COLx8AU2
MrCCohE1X8pszkrMfTfy2XCy0cXLeoJdSnEeZa/WBhUbBVPWOtzUpnzLdad/7576
tMKrs5DHoWT3g9ltfa5l4ExgntBM9gvb/ieNqY2bhJ4CtSIfgRgCz3EYh25dop1m
0O9ipBWcGJT9Vsx8vP+jSLl1bXhHzWbye8zA7ehMl8HeFxvv4JzaGC68tW4ZH6u5
UmYTQJU8jSOcryZ3TIY=
-----END CERTIFICATE-----
"#;

pub(super) static CLIENT_CERT_EXTERNAL: &str = r#"
Certificate:
    Data:
        Version: 3 (0x2)
        Serial Number:
            02:d3:93:cc:ce:3f:bf:6d:92:27:ba:70:4d:91:77:2d:99:7e:1b:35
        Signature Algorithm: sha256WithRSAEncryption
        Issuer: CN = usercert-ca.example.com
        Validity
            Not Before: Oct 16 18:33:20 2024 GMT
            Not After : Oct 16 18:38:50 2024 GMT
        Subject: O = ExampleCo, OU = admins, CN = testuser
        Subject Public Key Info:
            Public Key Algorithm: rsaEncryption
                Public-Key: (2048 bit)
                Modulus:
                    00:ed:21:3d:55:a7:f7:60:4a:f8:94:1e:c2:e4:d2:
                    1e:d8:3e:2d:64:8d:c7:f8:68:32:a6:f6:7f:95:e4:
                    c6:17:d4:8d:07:ec:36:d9:26:2e:dc:96:2e:75:e4:
                    b2:19:95:34:ef:f5:ed:43:61:bd:0f:92:1f:06:a2:
                    08:8d:aa:8a:7d:8a:fc:3e:1e:42:e5:83:e7:4e:ea:
                    55:d7:6f:54:cb:95:79:bd:39:fc:f0:6b:db:f2:3c:
                    a0:a5:9e:d9:18:d4:e3:52:c9:14:82:0e:c8:29:03:
                    40:69:23:de:5e:05:cb:ab:8b:e3:5b:26:ed:78:64:
                    32:24:d9:f6:6a:15:f9:a2:ee:4c:14:28:6d:fd:f0:
                    be:56:b7:53:27:7a:67:2c:5d:58:64:7e:0a:02:14:
                    4f:90:c4:80:0b:c7:ba:f1:e7:23:a7:4b:7e:4a:aa:
                    b1:5e:0c:a0:bf:dc:59:98:e0:f7:77:a9:9b:3b:11:
                    87:b8:60:d2:f2:b5:c8:36:22:45:f9:9c:db:0d:49:
                    91:5d:a4:c1:eb:14:a0:0a:6f:e9:e7:79:af:1f:96:
                    1a:22:5f:a9:f5:47:53:bd:c5:37:30:8c:ad:c6:2d:
                    8b:1f:26:45:b8:26:e1:2a:8b:57:56:f1:10:b6:a2:
                    47:22:ab:75:07:3d:a0:f5:a9:13:62:cd:f0:c3:56:
                    29:c3
                Exponent: 65537 (0x10001)
        X509v3 extensions:
            X509v3 Key Usage: critical
                Digital Signature, Key Encipherment, Key Agreement
            X509v3 Extended Key Usage:
                TLS Web Client Authentication
            X509v3 Subject Key Identifier:
                18:03:B3:90:8C:27:6E:36:C5:25:3A:B8:48:20:FD:4B:9C:F5:88:CC
            X509v3 Authority Key Identifier:
                CC:A0:1C:2A:C1:0D:C1:99:0B:A8:4B:D0:8D:22:4A:9C:C6:4E:27:17
            Authority Information Access:
                CA Issuers - URI:https://ca.example.test/ca
            X509v3 Subject Alternative Name:
                DNS:testuser
            X509v3 CRL Distribution Points:
                Full Name:
                  URI:http://ca.example.test/crl.pem
    Signature Algorithm: sha256WithRSAEncryption
    Signature Value:
        85:41:cc:37:10:21:33:58:47:0d:d3:80:23:b4:9b:7e:8b:2d:
        a4:20:1d:e7:bb:c4:2a:35:73:4a:ac:12:5a:be:d1:f4:73:79:
        1a:21:9f:fe:be:fc:ac:3d:ea:22:eb:27:c1:52:91:3b:1a:96:
        89:34:6c:71:93:bb:74:fd:f2:3a:81:ed:46:56:2d:c1:e9:31:
        17:f1:21:ea:49:6a:81:ba:95:01:cd:d4:56:cc:96:e2:dc:43:
        77:62:18:5a:fe:d2:78:a9:32:0b:4c:c6:83:b6:c0:a7:d1:56:
        54:47:2c:47:5e:6a:ef:5e:ce:84:a9:d9:bf:fe:09:b2:28:59:
        ff:8c:95:1d:51:e4:ff:4b:12:d2:91:53:a5:e1:54:4f:19:d9:
        d2:08:46:fb:4b:dd:2b:94:99:32:18:b4:4c:d6:ea:a0:e3:f2:
        78:e9:d4:da:0a:7b:8b:19:b5:57:9c:20:09:83:a9:d0:be:14:
        4b:0c:00:8d:23:82:6b:84:9c:7d:27:80:d5:cd:4d:c9:3a:b6:
        e2:9d:a3:89:c6:ba:54:00:2c:bb:c3:99:9f:e6:44:99:1c:1a:
        7c:9d:25:65:cb:7f:63:15:c7:86:0c:88:36:4f:ce:f6:6a:95:
        ca:60:f4:6f:e9:25:45:4c:5e:99:59:65:0b:f1:71:00:cc:cc:
        64:aa:73:52
-----BEGIN CERTIFICATE-----
MIIDzzCCAregAwIBAgIUAtOTzM4/v22SJ7pwTZF3LZl+GzUwDQYJKoZIhvcNAQEL
BQAwIjEgMB4GA1UEAxMXdXNlcmNlcnQtY2EuZXhhbXBsZS5jb20wHhcNMjQxMDE2
MTgzMzIwWhcNMjQxMDE2MTgzODUwWjA4MRIwEAYDVQQKEwlFeGFtcGxlQ28xDzAN
BgNVBAsTBmFkbWluczERMA8GA1UEAxMIdGVzdHVzZXIwggEiMA0GCSqGSIb3DQEB
AQUAA4IBDwAwggEKAoIBAQDtIT1Vp/dgSviUHsLk0h7YPi1kjcf4aDKm9n+V5MYX
1I0H7DbZJi7cli515LIZlTTv9e1DYb0Pkh8GogiNqop9ivw+HkLlg+dO6lXXb1TL
lXm9Ofzwa9vyPKClntkY1ONSyRSCDsgpA0BpI95eBcuri+NbJu14ZDIk2fZqFfmi
7kwUKG398L5Wt1MnemcsXVhkfgoCFE+QxIALx7rx5yOnS35KqrFeDKC/3FmY4Pd3
qZs7EYe4YNLytcg2IkX5nNsNSZFdpMHrFKAKb+nnea8flhoiX6n1R1O9xTcwjK3G
LYsfJkW4JuEqi1dW8RC2okciq3UHPaD1qRNizfDDVinDAgMBAAGjgeYwgeMwDgYD
VR0PAQH/BAQDAgOoMBMGA1UdJQQMMAoGCCsGAQUFBwMCMB0GA1UdDgQWBBQYA7OQ
jCduNsUlOrhIIP1LnPWIzDAfBgNVHSMEGDAWgBTMoBwqwQ3BmQuoS9CNIkqcxk4n
FzA2BggrBgEFBQcBAQQqMCgwJgYIKwYBBQUHMAKGGmh0dHBzOi8vY2EuZXhhbXBs
ZS50ZXN0L2NhMBMGA1UdEQQMMAqCCHRlc3R1c2VyMC8GA1UdHwQoMCYwJKAioCCG
Hmh0dHA6Ly9jYS5leGFtcGxlLnRlc3QvY3JsLnBlbTANBgkqhkiG9w0BAQsFAAOC
AQEAhUHMNxAhM1hHDdOAI7SbfostpCAd57vEKjVzSqwSWr7R9HN5GiGf/r78rD3q
IusnwVKROxqWiTRscZO7dP3yOoHtRlYtwekxF/Eh6klqgbqVAc3UVsyW4txDd2IY
Wv7SeKkyC0zGg7bAp9FWVEcsR15q717OhKnZv/4JsihZ/4yVHVHk/0sS0pFTpeFU
TxnZ0ghG+0vdK5SZMhi0TNbqoOPyeOnU2gp7ixm1V5wgCYOp0L4USwwAjSOCa4Sc
fSeA1c1NyTq24p2jica6VAAsu8OZn+ZEmRwafJ0lZct/YxXHhgyINk/O9mqVymD0
b+klRUxemVllC/FxAMzMZKpzUg==
-----END CERTIFICATE-----
"#;

pub(super) static CLIENT_CERT_MACHINEATRON: &str = r#"
Certificate:
    Data:
        Version: 3 (0x2)
        Serial Number:
            48:07:3a:dd:d0:15:ed:26:e1:4d:15:59:e9:10:8c:54:8d:9c:53:bf
        Signature Algorithm: sha256WithRSAEncryption
        Issuer: CN = site-root
        Validity
            Not Before: Oct 29 20:59:57 2024 GMT
            Not After : Nov 28 21:00:27 2024 GMT
        Subject:
        Subject Public Key Info:
            Public Key Algorithm: id-ecPublicKey
                Public-Key: (384 bit)
                pub:
                    04:9b:62:af:1e:17:a6:96:81:8a:2d:fe:e7:93:c3:
                    e3:89:65:f0:21:ec:ac:aa:94:3e:09:7e:ab:2c:29:
                    9d:7d:5c:a5:de:70:79:76:8b:de:2d:87:90:d8:39:
                    8d:50:2a:96:32:16:a6:23:ef:7d:3c:a9:a5:51:e5:
                    76:eb:fb:af:17:ed:26:af:6c:a1:50:31:8b:af:50:
                    08:d3:95:e6:c8:76:07:e4:3d:29:ae:1d:aa:09:5a:
                    18:ee:84:1e:8b:5f:c0
                ASN1 OID: secp384r1
                NIST CURVE: P-384
        X509v3 extensions:
            X509v3 Key Usage: critical
                Digital Signature, Key Encipherment, Key Agreement
            X509v3 Extended Key Usage:
                TLS Web Server Authentication, TLS Web Client Authentication
            X509v3 Subject Key Identifier:
                C5:8D:98:8A:7C:10:24:AF:2A:86:FA:68:86:F7:49:3C:D3:1E:C0:E2
            X509v3 Authority Key Identifier:
                F6:BF:5F:8F:F7:08:E8:C5:AE:E9:74:60:1D:DD:AB:D3:28:31:50:48
            X509v3 Subject Alternative Name: critical
                DNS:machine-a-tron.carbide-system.svc.cluster.local, URI:spiffe://example.test/carbide-system/sa/machine-a-tron
    Signature Algorithm: sha256WithRSAEncryption
    Signature Value:
        8a:f2:fa:dc:b8:45:12:8d:2e:b3:a0:ca:80:2b:a9:2d:e5:ba:
        ce:5b:84:71:fa:d3:c5:e1:0d:60:b1:02:0a:87:20:63:39:db:
        77:2a:aa:e2:ed:e6:9a:1b:1f:ac:7a:64:ad:0e:fa:23:1c:27:
        cf:1a:db:fa:79:03:89:8d:2b:0f:0e:4d:df:44:2d:1d:99:76:
        58:a8:52:e7:00:d1:df:98:b2:5c:75:bf:31:38:4e:31:8c:c7:
        78:1b:cf:1b:84:01:38:c8:e1:1a:02:be:be:6a:0a:f2:55:f6:
        ee:75:ae:7c:8f:68:6f:e4:3f:8c:b3:0e:c2:74:46:0b:7e:da:
        bc:0b:24:15:86:e8:2a:01:0f:ea:40:c7:63:b4:21:9d:ea:05:
        5c:29:f3:9b:2c:10:50:0e:9c:cf:f4:ec:96:3e:d3:9d:4d:14:
        75:55:71:fa:22:cd:18:63:5a:aa:19:f0:4f:24:c1:9a:cd:80:
        68:48:23:5a:38:31:0c:2f:d7:f1:39:6c:08:44:da:c8:27:60:
        50:d4:83:25:38:d4:00:de:1f:10:d5:c4:95:a1:9e:ec:72:b4:
        58:dd:ce:db:29:84:9e:f8:86:5b:b1:86:b8:aa:68:98:60:41:
        4b:8d:66:eb:9c:7f:c9:cd:3f:e7:05:18:f7:08:93:41:b5:04:
        ed:32:c4:19
-----BEGIN CERTIFICATE-----
MIIC3jCCAcagAwIBAgIUSAc63dAV7SbhTRVZ6RCMVI2cU78wDQYJKoZIhvcNAQEL
BQAwFDESMBAGA1UEAxMJc2l0ZS1yb290MB4XDTI0MTAyOTIwNTk1N1oXDTI0MTEy
ODIxMDAyN1owADB2MBAGByqGSM49AgEGBSuBBAAiA2IABJtirx4XppaBii3+55PD
44ll8CHsrKqUPgl+qywpnX1cpd5weXaL3i2HkNg5jVAqljIWpiPvfTyppVHlduv7
rxftJq9soVAxi69QCNOV5sh2B+Q9Ka4dqglaGO6EHotfwKOB6TCB5jAOBgNVHQ8B
Af8EBAMCA6gwHQYDVR0lBBYwFAYIKwYBBQUHAwEGCCsGAQUFBwMCMB0GA1UdDgQW
BBTFjZiKfBAkryqG+miG90k80x7A4jAfBgNVHSMEGDAWgBT2v1+P9wjoxa7pdGAd
3avTKDFQSDB1BgNVHREBAf8EazBpgi9tYWNoaW5lLWEtdHJvbi5jYXJiaWRlLXN5
c3RlbS5zdmMuY2x1c3Rlci5sb2NhbIY2c3BpZmZlOi8vZXhhbXBsZS50ZXN0L2Nh
cmJpZGUtc3lzdGVtL3NhL21hY2hpbmUtYS10cm9uMA0GCSqGSIb3DQEBCwUAA4IB
AQCK8vrcuEUSjS6zoMqAK6kt5brOW4Rx+tPF4Q1gsQIKhyBjOdt3Kqri7eaaGx+s
emStDvojHCfPGtv6eQOJjSsPDk3fRC0dmXZYqFLnANHfmLJcdb8xOE4xjMd4G88b
hAE4yOEaAr6+agryVfbuda58j2hv5D+Msw7CdEYLftq8CyQVhugqAQ/qQMdjtCGd
6gVcKfObLBBQDpzP9OyWPtOdTRR1VXH6Is0YY1qqGfBPJMGazYBoSCNaODEML9fx
OWwIRNrIJ2BQ1IMlONQA3h8Q1cSVoZ7scrRY3c7bKYSe+IZbsYa4qmiYYEFLjWbr
nH/JzT/nBRj3CJNBtQTtMsQZ
-----END CERTIFICATE-----
"#;

pub(super) static CLIENT_CERT_OTHER_APP: &str = r#"
Certificate:
    Data:
        Version: 3 (0x2)
        Serial Number:
            6b:9a:4c:2b:c2:67:61:5d:12:d6:25:1d:4c:5f:48:7f:e2:7a:f8:96
        Signature Algorithm: sha256WithRSAEncryption
        Issuer: O = ExampleCo, CN = ExampleCo Intermediate CA
        Validity
            Not Before: Oct 29 14:31:26 2024 GMT
            Not After : Nov 28 14:31:56 2024 GMT
        Subject:
        Subject Public Key Info:
            Public Key Algorithm: id-ecPublicKey
                Public-Key: (384 bit)
                pub:
                    04:a1:e2:9c:50:53:15:67:c3:fc:3a:42:f7:9d:05:
                    f6:c2:4d:43:4d:01:d9:37:4d:c6:60:51:9c:82:d9:
                    f7:33:bf:20:1a:ef:d0:15:75:78:e4:0c:1a:0b:3d:
                    5e:c7:b6:61:9d:cd:7b:70:b6:92:06:32:0f:24:01:
                    ba:ff:8c:69:12:7c:17:2d:99:4e:df:42:d2:0e:f0:
                    8d:f2:76:d8:33:7b:ea:c6:95:a8:7d:bb:43:62:3d:
                    0b:47:9f:bd:de:bd:71
                ASN1 OID: secp384r1
                NIST CURVE: P-384
        X509v3 extensions:
            X509v3 Key Usage: critical
                Digital Signature, Key Encipherment, Key Agreement
            X509v3 Extended Key Usage:
                TLS Web Server Authentication, TLS Web Client Authentication
            X509v3 Subject Key Identifier:
                B0:D8:DC:94:8D:42:1A:EE:D3:81:B9:62:C5:C6:17:28:E1:4B:76:28
            X509v3 Authority Key Identifier:
                DE:90:AA:2F:81:34:02:CA:7F:6D:77:12:33:07:21:64:63:AA:1D:B4
            X509v3 Subject Alternative Name: critical
                DNS:other-app.other-namespace.svc.cluster.local, URI:spiffe://example.test/other-namespace/sa/other-app
    Signature Algorithm: sha256WithRSAEncryption
    Signature Value:
        d2:db:28:73:f4:85:70:86:32:2a:2e:6f:fd:1f:21:14:25:7f:
        6c:53:c7:c1:fe:30:4f:de:e7:2e:11:8b:b2:fe:24:6b:11:ea:
        98:f5:27:6a:e7:81:d3:08:0f:03:3e:38:4e:bb:0d:40:e5:05:
        be:4c:3b:d4:65:49:2f:35:03:d9:42:05:4a:09:c4:d9:6b:e6:
        7d:5c:49:f1:6e:22:2a:77:98:6b:1b:53:08:57:62:43:9f:c2:
        de:13:b3:ed:0d:2b:21:0d:91:61:89:c1:64:89:0c:9a:6a:23:
        35:72:78:ad:3e:dd:83:ec:f8:be:14:58:2c:5b:49:10:11:5b:
        29:13:e6:9d:4b:8a:ec:7b:b3:98:07:5e:72:2d:e1:71:5d:8c:
        8e:30:a0:a5:dd:75:38:74:2b:2f:36:88:e2:cd:31:c2:2e:3b:
        37:16:cb:78:20:2e:13:09:dc:94:00:24:1d:da:42:68:4c:05:
        21:be:cd:e3:6d:57:b4:b1:59:31:91:f1:e5:76:7e:d9:48:27:
        ba:38:7b:24:4a:b1:5e:fb:f2:b5:f4:cb:8a:01:66:27:d9:ff:
        36:9a:61:da:0b:65:dc:9b:c2:d7:26:2c:3c:59:e8:8f:8c:e2:
        21:5d:4f:43:57:7f:2e:65:12:2c:29:f3:d4:4a:12:b5:f9:d7:
        59:64:a6:d8
-----BEGIN CERTIFICATE-----
MIIC+jCCAeKgAwIBAgIUa5pMK8JnYV0S1iUdTF9If+J6+JYwDQYJKoZIhvcNAQEL
BQAwODESMBAGA1UEChMJRXhhbXBsZUNvMSIwIAYDVQQDExlFeGFtcGxlQ28gSW50
ZXJtZWRpYXRlIENBMB4XDTI0MTAyOTE0MzEyNloXDTI0MTEyODE0MzE1NlowADB2
MBAGByqGSM49AgEGBSuBBAAiA2IABKHinFBTFWfD/DpC950F9sJNQ00B2TdNxmBR
nILZ9zO/IBrv0BV1eOQMGgs9Xse2YZ3Ne3C2kgYyDyQBuv+MaRJ8Fy2ZTt9C0g7w
jfJ22DN76saVqH27Q2I9C0efvd69caOB4TCB3jAOBgNVHQ8BAf8EBAMCA6gwHQYD
VR0lBBYwFAYIKwYBBQUHAwEGCCsGAQUFBwMCMB0GA1UdDgQWBBSw2NyUjUIa7tOB
uWLFxhco4Ut2KDAfBgNVHSMEGDAWgBTekKovgTQCyn9tdxIzByFkY6odtDBtBgNV
HREBAf8EYzBhgitvdGhlci1hcHAub3RoZXItbmFtZXNwYWNlLnN2Yy5jbHVzdGVy
LmxvY2FshjJzcGlmZmU6Ly9leGFtcGxlLnRlc3Qvb3RoZXItbmFtZXNwYWNlL3Nh
L290aGVyLWFwcDANBgkqhkiG9w0BAQsFAAOCAQEA0tsoc/SFcIYyKi5v/R8hFCV/
bFPHwf4wT97nLhGLsv4kaxHqmPUnaueB0wgPAz44TrsNQOUFvkw71GVJLzUD2UIF
SgnE2WvmfVxJ8W4iKneYaxtTCFdiQ5/C3hOz7Q0rIQ2RYYnBZIkMmmojNXJ4rT7d
g+z4vhRYLFtJEBFbKRPmnUuK7HuzmAdeci3hcV2MjjCgpd11OHQrLzaI4s0xwi47
NxbLeCAuEwnclAAkHdpCaEwFIb7N421XtLFZMZHx5XZ+2Ugnujh7JEqxXvvytfTL
igFmJ9n/Npph2gtl3JvC1yYsPFnoj4ziIV1PQ1d/LmUSLCnz1EoStfnXWWSm2A==
-----END CERTIFICATE-----
Certificate:
    Data:
        Version: 3 (0x2)
        Serial Number:
            40:b2:35:8e:a9:9c:43:eb:21:00:d8:c2:42:3b:bc:2f:63:70:ae:03
        Signature Algorithm: sha256WithRSAEncryption
        Issuer: CN = Example Root Certificate Authority, O = ExampleCo, C = US
        Validity
            Not Before: Jun  1 13:47:49 2024 GMT
            Not After : Jun  1 13:48:19 2027 GMT
        Subject:
        Subject Public Key Info:
            Public Key Algorithm: id-ecPublicKey
                Public-Key: (256 bit)
                pub:
                    04:80:f4:f4:c6:47:f2:5d:01:74:54:d9:25:cd:45:
                    83:2f:00:f0:b2:c1:23:80:97:27:00:a7:93:f6:c0:
                    56:38:44:20:13:80:5b:b7:e2:63:21:28:9b:82:44:
                    cf:36:bc:28:0d:79:3c:f7:44:f0:6d:3a:24:1a:57:
                    c7:ef:e0:ae:9c
                ASN1 OID: prime256v1
                NIST CURVE: P-256
        X509v3 extensions:
            X509v3 Key Usage: critical
                Certificate Sign, CRL Sign
            X509v3 Basic Constraints: critical
                CA:TRUE
            X509v3 Subject Key Identifier:
                DE:90:AA:2F:81:34:02:CA:7F:6D:77:12:33:07:21:64:63:AA:1D:B4
            X509v3 Authority Key Identifier:
                8C:0B:BA:68:6E:54:94:89:AC:34:CF:6D:51:F9:B4:8A:8D:16:D0:1B
    Signature Algorithm: sha256WithRSAEncryption
    Signature Value:
        48:9c:23:22:f1:e4:f0:4d:01:b7:91:14:56:96:d7:e2:8c:3a:
        e1:7f:d1:be:9e:f6:10:29:dd:df:bf:4a:1c:5e:59:26:de:68:
        ae:04:10:fe:38:ea:b6:90:69:2a:a7:ea:3f:65:02:26:49:03:
        ce:c9:3d:34:d4:41:3f:92:bf:24:e9:45:c9:a4:c2:aa:dd:66:
        15:c4:e5:b2:ad:2f:2a:44:5f:96:b4:38:6e:29:c3:e9:b4:94:
        f6:9b:66:bf:c9:c8:1e:bf:03:a4:82:2c:a0:8b:2d:cb:04:16:
        5b:25:12:a8:0b:da:9b:67:a9:60:1f:5e:f0:82:04:c3:3a:ef:
        1a:4c:76:fb:d0:0f:e0:3b:eb:e8:57:e7:81:ed:19:bf:fa:ce:
        d2:b9:43:e0:27:91:e3:cc:7c:d6:2e:0e:c5:89:b8:f1:05:14:
        10:3e:ba:34:c7:04:53:43:94:dd:ba:65:de:a5:16:54:bf:ab:
        2d:77:88:96:e7:c2:7d:91:cd:48:30:89:6f:d2:22:59:03:04:
        34:d7:c8:9a:27:79:14:be:fb:9e:56:47:3c:b4:1c:b5:6f:7b:
        d3:a3:cb:1e:79:02:fd:d2:a6:3c:9f:a8:85:82:87:99:c3:64:
        ba:84:09:04:a3:b4:b8:cf:e2:76:aa:16:62:4c:92:90:67:61:
        4d:0c:03:e1
-----BEGIN CERTIFICATE-----
MIICdDCCAVygAwIBAgIUQLI1jqmcQ+shANjCQju8L2NwrgMwDQYJKoZIhvcNAQEL
BQAwTjErMCkGA1UEAwwiRXhhbXBsZSBSb290IENlcnRpZmljYXRlIEF1dGhvcml0
eTESMBAGA1UECgwJRXhhbXBsZUNvMQswCQYDVQQGEwJVUzAeFw0yNDA2MDExMzQ3
NDlaFw0yNzA2MDExMzQ4MTlaMAAwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAASA
9PTGR/JdAXRU2SXNRYMvAPCywSOAlycAp5P2wFY4RCATgFu34mMhKJuCRM82vCgN
eTz3RPBtOiQaV8fv4K6co2MwYTAOBgNVHQ8BAf8EBAMCAQYwDwYDVR0TAQH/BAUw
AwEB/zAdBgNVHQ4EFgQU3pCqL4E0Asp/bXcSMwchZGOqHbQwHwYDVR0jBBgwFoAU
jAu6aG5UlImsNM9tUfm0io0W0BswDQYJKoZIhvcNAQELBQADggEBAEicIyLx5PBN
AbeRFFaW1+KMOuF/0b6e9hAp3d+/ShxeWSbeaK4EEP446raQaSqn6j9lAiZJA87J
PTTUQT+SvyTpRcmkwqrdZhXE5bKtLypEX5a0OG4pw+m0lPabZr/JyB6/A6SCLKCL
LcsEFlslEqgL2ptnqWAfXvCCBMM67xpMdvvQD+A76+hX54HtGb/6ztK5Q+AnkePM
fNYuDsWJuPEFFBA+ujTHBFNDlN26Zd6lFlS/qy13iJbnwn2RzUgwiW/SIlkDBDTX
yJoneRS++55WRzy0HLVve9Ojyx55Av3SpjyfqIWCh5nDZLqECQSjtLjP4naqFmJM
kpBnYU0MA+E=
-----END CERTIFICATE-----
"#;

pub(super) static CLIENT_CERT_CI: &str = r#"
Certificate:
    Data:
        Version: 3 (0x2)
        Serial Number:
            03:9b:f6:83:19:d3:7e:4a:f3:e6:17:78:84:99:8b:26:84:39:8e:4a
        Signature Algorithm: sha256WithRSAEncryption
        Issuer: CN = Example Root Certificate Authority, O = ExampleCo, C = US
        Validity
            Not Before: Nov 26 14:40:21 2024 GMT
            Not After : Nov 26 20:40:51 2024 GMT
        Subject: OU = generic ci/cd, CN = ci-host.example.com
        Subject Public Key Info:
            Public Key Algorithm: id-ecPublicKey
                Public-Key: (384 bit)
                pub:
                    04:35:66:00:ff:46:d9:30:be:2e:5b:5b:d6:00:07:
                    e9:0a:06:19:c9:d7:3b:a8:1f:24:5c:cb:fc:4f:dc:
                    84:9f:6d:0a:ae:38:2b:65:8c:a9:97:2c:0a:8b:84:
                    3f:d8:25:a9:8a:22:e1:bb:f6:36:87:f8:28:1a:31:
                    bf:94:ae:9a:60:d3:d0:79:93:a5:7e:8b:f7:f3:89:
                    a4:c1:67:3e:2c:8b:9b:8f:b2:6f:5d:1f:b4:a0:be:
                    0a:01:31:40:fc:f4:7e
                ASN1 OID: secp384r1
                NIST CURVE: P-384
        X509v3 extensions:
            X509v3 Key Usage: critical
                Digital Signature, Key Encipherment, Key Agreement
            X509v3 Extended Key Usage:
                TLS Web Server Authentication, TLS Web Client Authentication
            X509v3 Subject Key Identifier:
                F8:78:D6:10:E8:D7:D1:69:80:BB:C9:CC:A9:D1:5A:23:C1:C1:FF:04
            X509v3 Authority Key Identifier:
                8C:0B:BA:68:6E:54:94:89:AC:34:CF:6D:51:F9:B4:8A:8D:16:D0:1B
            X509v3 Subject Alternative Name:
                DNS:ci-host.example.com
    Signature Algorithm: sha256WithRSAEncryption
    Signature Value:
        2c:13:27:57:ad:61:92:81:2c:fa:d2:9b:44:44:18:6c:dc:d5:
        b8:d7:02:7e:a9:86:60:4f:ab:86:e0:30:3f:84:77:b0:28:9f:
        e0:23:7e:07:5b:6a:59:17:df:f1:bd:dc:08:19:7a:36:66:58:
        e0:3e:3f:b3:36:a0:cd:6c:8a:63:d5:4c:6d:cb:6a:57:a3:00:
        c6:ca:36:7f:ba:cb:97:af:9f:4b:a6:0d:d5:6d:ee:d7:ab:7f:
        12:2c:0b:31:ea:05:1c:af:1c:58:cf:d4:b4:5c:0b:24:69:8d:
        33:36:b8:fc:74:c9:e9:11:97:6c:91:64:5b:be:a0:3f:12:54:
        ed:e9:ec:f0:e4:6e:5c:18:a2:c6:89:62:34:10:f0:67:d9:d6:
        ec:60:9e:d9:a9:10:38:b5:5a:8c:13:3f:8f:a8:f8:c5:16:e0:
        ed:94:3e:bd:c7:c6:27:e8:c6:d4:ff:55:da:b3:ee:93:e1:df:
        38:a2:ad:e0:b1:f6:05:35:b8:48:cc:6b:bb:5d:f7:41:b9:6f:
        a4:0d:02:17:a7:5e:0b:d8:08:9e:82:6c:7d:eb:28:20:10:66:
        44:1c:0f:d0:cd:5b:00:25:f5:b7:ab:85:59:63:8e:8c:df:59:
        d7:de:0d:3b:60:26:b1:68:36:a5:2b:d4:d2:d3:9f:9a:2e:e8:
        62:31:31:12
-----BEGIN CERTIFICATE-----
MIIC9zCCAd+gAwIBAgIUA5v2gxnTfkrz5hd4hJmLJoQ5jkowDQYJKoZIhvcNAQEL
BQAwTjErMCkGA1UEAxMiRXhhbXBsZSBSb290IENlcnRpZmljYXRlIEF1dGhvcml0
eTESMBAGA1UEChMJRXhhbXBsZUNvMQswCQYDVQQGEwJVUzAeFw0yNDExMjYxNDQw
MjFaFw0yNDExMjYyMDQwNTFaMDYxFjAUBgNVBAsTDWdlbmVyaWMgY2kvY2QxHDAa
BgNVBAMTE2NpLWhvc3QuZXhhbXBsZS5jb20wdjAQBgcqhkjOPQIBBgUrgQQAIgNi
AAQ1ZgD/Rtkwvi5bW9YAB+kKBhnJ1zuoHyRcy/xP3ISfbQquOCtljKmXLAqLhD/Y
JamKIuG79jaH+CgaMb+Urppg09B5k6V+i/fziaTBZz4si5uPsm9dH7SgvgoBMUD8
9H6jgZIwgY8wDgYDVR0PAQH/BAQDAgOoMB0GA1UdJQQWMBQGCCsGAQUFBwMBBggr
BgEFBQcDAjAdBgNVHQ4EFgQU+HjWEOjX0WmAu8nMqdFaI8HB/wQwHwYDVR0jBBgw
FoAUjAu6aG5UlImsNM9tUfm0io0W0BswHgYDVR0RBBcwFYITY2ktaG9zdC5leGFt
cGxlLmNvbTANBgkqhkiG9w0BAQsFAAOCAQEALBMnV61hkoEs+tKbREQYbNzVuNcC
fqmGYE+rhuAwP4R3sCif4CN+B1tqWRff8b3cCBl6NmZY4D4/szagzWyKY9VMbctq
V6MAxso2f7rLl6+fS6YN1W3u16t/EiwLMeoFHK8cWM/UtFwLJGmNMza4/HTJ6RGX
bJFkW76gPxJU7ens8ORuXBiixoliNBDwZ9nW7GCe2akQOLVajBM/j6j4xRbg7ZQ+
vcfGJ+jG1P9V2rPuk+HfOKKt4LH2BTW4SMxru133QblvpA0CF6deC9gInoJsfeso
IBBmRBwP0M1bACX1t6uFWWOOjN9Z194NO2AmsWg2pSvU0tOfmi7oYjExEg==
-----END CERTIFICATE-----
"#;
