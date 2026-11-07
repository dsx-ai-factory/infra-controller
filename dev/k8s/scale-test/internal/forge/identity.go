// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package forge is the nico-api (forge.Forge) client the runner writes with. nico-api lets the machine-a-tron service call the expected-inventory Add* RPCs, so the
// client certificate's SPIFFE ID must classify as that service under the site's SPIFFE settings. nico-api accepts a TLS client with no certificate, so the key must load
// and match the certificate before any call.
package forge

import (
	"crypto/tls"
	"crypto/x509"
	"encoding/asn1"
	"encoding/pem"
	"errors"
	"fmt"
	"net/url"
	"os"
	"strings"
)

// WriteService is the service nico-api authorizes for expected-inventory writes.
const WriteService = "machine-a-tron"

// SpiffeSettings is the configured nico-api SPIFFE settings.
type SpiffeSettings struct {
	TrustDomain      string   `yaml:"trustDomain"`
	ServiceBasePaths []string `yaml:"serviceBasePaths"`
	MachineBasePath  string   `yaml:"machineBasePath"`
}

// spiffeID is a parsed SPIFFE ID: its trust domain and path.
type spiffeID struct {
	trustDomain, path, full string
}

// CheckIdentity returns the SPIFFE ID of the first certificate in certPEM when it names WriteService under spiffe. The error never repeats the configured trust domain or
// the certificate's SPIFFE ID.
func CheckIdentity(certPEM []byte, spiffe SpiffeSettings) (string, error) {
	leaf, err := firstCert(certPEM)
	if err != nil {
		return "", err
	}
	id, err := certIdentity(leaf)
	if err != nil {
		return "", err
	}
	td, err := trustDomainName(spiffe.TrustDomain)
	if err != nil {
		return "", err
	}
	if id.trustDomain != td {
		return "", errors.New("the SPIFFE ID is not in the configured trust domain")
	}
	kind, ident, ok := classify(id.path, spiffe)
	switch {
	case !ok:
		return "", errors.New("the SPIFFE ID names no service or machine under the configured base paths")
	case kind == "service" && ident == WriteService:
		return id.full, nil
	case kind == "service":
		return "", errors.New("the SPIFFE ID names another service, not " + WriteService)
	default:
		return "", errors.New("the SPIFFE ID names a machine, not service " + WriteService)
	}
}

// classify strips the first service base path, else the machine base path, that prefixes the SPIFFE ID path. The remainder is the identifier; an empty one does not
// classify, and a service-path match is never read as a machine.
func classify(path string, spiffe SpiffeSettings) (kind, ident string, ok bool) {
	for _, base := range spiffe.ServiceBasePaths {
		if rest, found := strings.CutPrefix(path, base); found {
			return "service", rest, rest != ""
		}
	}
	if rest, found := strings.CutPrefix(path, spiffe.MachineBasePath); found {
		return "machine", rest, rest != ""
	}
	return "", "", false
}

// trustDomainName is the configured trust domain, given as a bare name or as a spiffe:// ID. The error never repeats it.
func trustDomainName(raw string) (string, error) {
	if !isASCII(raw) {
		return "", errors.New("the configured trust domain is not ASCII")
	}
	if raw == "" {
		return "", errors.New("the configured trust domain is not valid: it is empty")
	}
	if strings.Contains(raw, ":/") { // a scheme separator means a SPIFFE ID
		id, err := parseSpiffeURL(raw)
		if err != nil {
			return "", errors.New("the configured trust domain is not valid: not a SPIFFE ID")
		}
		return id.trustDomain, nil
	}
	if !validTrustDomain(raw) {
		return "", errors.New("the configured trust domain is not valid: a disallowed character")
	}
	return raw, nil
}

// certIdentity is the SPIFFE ID in a leaf certificate: exactly one URI SAN, a valid spiffe:// ID. The raw SAN string is checked before url.Parse can normalize it
// (lowercase the scheme, drop an empty trailing fragment). The leaf must not be a CA or sign certificates/CRLs. No error repeats the ID.
func certIdentity(leaf *x509.Certificate) (spiffeID, error) {
	raws := rawSANURIs(leaf)
	switch {
	case leaf.IsCA:
		return spiffeID{}, errors.New("the certificate is a CA, not a leaf")
	case leaf.KeyUsage&x509.KeyUsageCertSign != 0:
		return spiffeID{}, errors.New("the certificate may sign certificates")
	case leaf.KeyUsage&x509.KeyUsageCRLSign != 0:
		return spiffeID{}, errors.New("the certificate may sign CRLs")
	case len(raws) != 1:
		return spiffeID{}, fmt.Errorf("the certificate must have exactly one URI SAN, it has %d", len(raws))
	case !strings.HasPrefix(raws[0], "spiffe://"):
		return spiffeID{}, errors.New("the SPIFFE ID scheme is not spiffe")
	case strings.ContainsRune(raws[0], '#'):
		return spiffeID{}, errors.New("the SPIFFE ID has a fragment")
	}
	u, err := url.Parse(raws[0])
	if err != nil {
		return spiffeID{}, errors.New("the SPIFFE ID does not parse")
	}
	return parseSpiffe(u)
}

// rawSANURIs is the raw uniformResourceIdentifier strings of the leaf's subjectAltName extension (OID 2.5.29.17), before url.Parse sees them.
func rawSANURIs(leaf *x509.Certificate) []string {
	var uris []string
	for _, ext := range leaf.Extensions {
		if !ext.Id.Equal([]int{2, 5, 29, 17}) {
			continue
		}
		var seq asn1.RawValue
		if _, err := asn1.Unmarshal(ext.Value, &seq); err != nil {
			return nil
		}
		for rest := seq.Bytes; len(rest) > 0; {
			var gn asn1.RawValue
			var err error
			if rest, err = asn1.Unmarshal(rest, &gn); err != nil {
				return nil
			}
			if gn.Class == asn1.ClassContextSpecific && gn.Tag == 6 {
				uris = append(uris, string(gn.Bytes))
			}
		}
	}
	return uris
}

func parseSpiffeURL(raw string) (spiffeID, error) {
	u, err := url.Parse(raw)
	if err != nil {
		return spiffeID{}, err
	}
	return parseSpiffe(u)
}

// parseSpiffe reads a URI as a SPIFFE ID by the raw rules of authn/spiffe_id.rs: scheme spiffe, a valid trust-domain host with no port, no userinfo, query or fragment,
// and valid unescaped path segments. A malformed ID is never normalized into a valid one; no error repeats the ID.
func parseSpiffe(u *url.URL) (spiffeID, error) {
	path := u.EscapedPath() // the raw path; a percent escape stays, so it is rejected
	switch {
	case u.Scheme != "spiffe":
		return spiffeID{}, errors.New("the SPIFFE ID scheme is not spiffe")
	case u.User != nil:
		return spiffeID{}, errors.New("the SPIFFE ID carries userinfo")
	case u.ForceQuery || u.RawQuery != "":
		return spiffeID{}, errors.New("the SPIFFE ID has a query")
	case u.Fragment != "" || u.RawFragment != "":
		return spiffeID{}, errors.New("the SPIFFE ID has a fragment")
	case u.Opaque != "" || u.Port() != "" || u.Host == "" || !validTrustDomain(u.Host):
		return spiffeID{}, errors.New("the SPIFFE ID trust domain is missing or invalid")
	case !validPath(path):
		return spiffeID{}, errors.New("the SPIFFE ID path is not valid")
	}
	return spiffeID{trustDomain: u.Host, path: path, full: "spiffe://" + u.Host + path}, nil
}

// validPath mirrors authn/spiffe_id.rs validate_path: an empty path is allowed, else each '/'-led segment holds only [A-Za-z0-9-._] and is not "", "." or "..".
func validPath(path string) bool {
	if path == "" {
		return true
	}
	if path[0] != '/' {
		return false
	}
	for _, seg := range strings.Split(path[1:], "/") {
		if seg == "" || seg == "." || seg == ".." || strings.ContainsFunc(seg, func(r rune) bool { return !pathChar(r) }) {
			return false
		}
	}
	return true
}

func pathChar(r rune) bool {
	return r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || r == '-' || r == '.' || r == '_'
}

// validTrustDomain reports a non-empty name of trust-domain characters.
func validTrustDomain(name string) bool {
	if name == "" {
		return false
	}
	return !strings.ContainsFunc(name, func(r rune) bool {
		return (r < 'a' || r > 'z') && (r < '0' || r > '9') && r != '-' && r != '.' && r != '_'
	})
}

func isASCII(s string) bool {
	return !strings.ContainsFunc(s, func(r rune) bool { return r > 0x7f })
}

// firstCert parses the first CERTIFICATE block in pemBytes.
func firstCert(pemBytes []byte) (*x509.Certificate, error) {
	for rest := pemBytes; len(rest) > 0; {
		var block *pem.Block
		block, rest = pem.Decode(rest)
		if block == nil {
			break
		}
		if block.Type == "CERTIFICATE" {
			leaf, err := x509.ParseCertificate(block.Bytes)
			if err != nil {
				return nil, errors.New("does not parse: the certificate is not valid DER")
			}
			return leaf, nil
		}
	}
	return nil, errors.New("holds no certificate")
}

// CheckKeyPair checks that keyPEM holds the private key of the first cert in certPEM; as the nico-api client reads it, its first PEM item must be the key.
func CheckKeyPair(certPEM, keyPEM []byte) error {
	if block, _ := pem.Decode(keyPEM); block == nil || !strings.HasSuffix(block.Type, "PRIVATE KEY") {
		return errors.New("the key file does not start with a private key")
	}
	if _, err := tls.X509KeyPair(certPEM, keyPEM); err != nil {
		return errors.New("the key is not the certificate's")
	}
	return nil
}

// CheckKeyPairFiles reads the client certificate and key and checks that they belong together. The error starts with the key file path.
func CheckKeyPairFiles(certPath, keyPath string) error {
	cert, err := readFile(certPath)
	if err != nil {
		return err
	}
	key, err := readFile(keyPath)
	if err != nil {
		return err
	}
	if err := CheckKeyPair(cert, key); err != nil {
		return fmt.Errorf("%s: %w", keyPath, err)
	}
	return nil
}

func readFile(path string) ([]byte, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		// os error text holds the path; keep only the path and a fixed cause.
		if os.IsNotExist(err) {
			return nil, fmt.Errorf("%s: no such file", path)
		}
		return nil, fmt.Errorf("%s: could not be read", path)
	}
	return data, nil
}
