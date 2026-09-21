#!/bin/bash
#
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
# http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
#
set -euo pipefail

# Generate openssl.cnf for v3 extensions.
# IP SANs cover both Docker Desktop (192.168.65.254) and Colima (192.168.5.2)
# so the cert works regardless of container runtime if needed.
cat > openssl.cnf <<EOF
[ req ]
distinguished_name = req_distinguished_name
x509_extensions = v3_ca
req_extensions = v3_req
prompt = no

[ req_distinguished_name ]
CN = localhost

[ v3_ca ]
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid:always,issuer
basicConstraints = critical, CA:true
keyUsage = critical, digitalSignature, keyCertSign, cRLSign

[ v3_req ]
basicConstraints = CA:FALSE
keyUsage = digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth, clientAuth
subjectAltName = @alt_names

[ v3_ext ]
basicConstraints = CA:FALSE
keyUsage = digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth, clientAuth
subjectAltName = @alt_names

[ alt_names ]
DNS.1 = localhost
DNS.2 = host.docker.internal
IP.1 = 127.0.0.1
IP.2 = 192.168.65.254
IP.3 = 192.168.5.2
IP.4 = ::1
EOF

# Generate CA key and self-signed certificate
test -e ca.key || openssl ecparam -name prime256v1 -genkey -noout -out ca.key
if [[ ! -e ca.crt || ca.key -nt ca.crt ]]
then
    openssl req -x509 -new -key ca.key -sha256 -days 3650 -out ca.crt -config openssl.cnf -extensions v3_ca
fi

# Generate localhost.key and CSR
test -e localhost.key || openssl ecparam -name prime256v1 -genkey -noout -out localhost.key
if [[ ! -e localhost.crt || localhost.key -nt localhost.crt ]]
then
    openssl req -new -key localhost.key -out tls.csr -subj "/CN=localhost"
    # Sign server certificate with CA
    openssl x509 -req -in tls.csr -CA ca.crt -CAkey ca.key -CAcreateserial \
        -out localhost.crt -days 365 -sha256 \
        -extfile openssl.cnf -extensions v3_ext
fi
chmod 600 localhost.key

# Generate client key and CSR
test -e client.key || openssl ecparam -name prime256v1 -genkey -noout -out client.key
if [[ ! -e client.crt || client.key -nt client.crt ]]
then
    openssl req -new -key client.key -out client.csr -subj "/CN=Test Client"
    # Sign client certificate with CA
    openssl x509 -req -in client.csr -CA ca.crt -CAkey ca.key -CAcreateserial \
        -out client.crt -days 365 -sha256 \
        -extfile openssl.cnf -extensions v3_ext
fi

# Clean up
rm -f *.csr *.srl openssl.cnf

echo "Certificates generated:"
ls -1 *.{crt,key}
