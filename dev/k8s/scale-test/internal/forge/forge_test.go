// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package forge

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/asn1"
	"encoding/pem"
	"math/big"
	"net/url"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
)

// certWithRawSAN signs a cert whose subjectAltName holds rawURI verbatim, so the raw string reaches CheckIdentity before url.Parse normalizes it (the leaf helper
// re-encodes the URI before signing).
func certWithRawSAN(t *testing.T, rawURI string) []byte {
	t.Helper()
	key, _ := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	uri, _ := asn1.Marshal(asn1.RawValue{Class: asn1.ClassContextSpecific, Tag: 6, Bytes: []byte(rawURI)})
	san, _ := asn1.Marshal(asn1.RawValue{Tag: asn1.TagSequence, IsCompound: true, Bytes: uri})
	tmpl := &x509.Certificate{
		SerialNumber: big.NewInt(1), KeyUsage: x509.KeyUsageDigitalSignature,
		ExtraExtensions: []pkix.Extension{{Id: asn1.ObjectIdentifier{2, 5, 29, 17}, Value: san}},
	}
	der, err := x509.CreateCertificate(rand.Reader, tmpl, tmpl, &key.PublicKey, key)
	if err != nil {
		t.Fatal(err)
	}
	return pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der})
}

func TestRawSANValidation(t *testing.T) {
	good := "spiffe://nico.local/ns/forge/sa/machine-a-tron"
	id, err := CheckIdentity(certWithRawSAN(t, good), settings)
	is(t, []any{id, errText(err)}, []any{good, ""}, "a valid raw SAN classifies")
	// url.Parse would lowercase the scheme and drop an empty trailing fragment; the raw check rejects both, and the reason never repeats the ID.
	for _, tc := range []struct{ uri, want string }{
		{"SPIFFE://nico.local/ns/forge/sa/machine-a-tron", "the SPIFFE ID scheme is not spiffe"},
		{good + "#", "the SPIFFE ID has a fragment"},
	} {
		_, err := CheckIdentity(certWithRawSAN(t, tc.uri), settings)
		is(t, errText(err), tc.want, tc.uri)
		if strings.Contains(errText(err), "nico.local") {
			t.Errorf("%s: error leaked the ID", tc.uri)
		}
	}
}

func is(t *testing.T, got, want any, what string) {
	t.Helper()
	if !reflect.DeepEqual(got, want) {
		t.Errorf("%s: got %v, want %v", what, got, want)
	}
}

// leaf makes a PEM cert with the given URI SAN plus the key PEM; edit mutates the template (CA flag, key usage, extra URIs).
func leaf(t *testing.T, uri string, edit func(*x509.Certificate)) (certPEM, keyPEM []byte, key *ecdsa.PrivateKey) {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	tmpl := &x509.Certificate{SerialNumber: big.NewInt(1), KeyUsage: x509.KeyUsageDigitalSignature}
	if uri != "" {
		u, err := url.Parse(uri)
		if err != nil {
			t.Fatal(err)
		}
		tmpl.URIs = []*url.URL{u}
	}
	if edit != nil {
		edit(tmpl)
	}
	der, err := x509.CreateCertificate(rand.Reader, tmpl, tmpl, &key.PublicKey, key)
	if err != nil {
		t.Fatal(err)
	}
	certPEM = pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der})
	keyDER, _ := x509.MarshalPKCS8PrivateKey(key)
	keyPEM = pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: keyDER})
	return certPEM, keyPEM, key
}

var settings = SpiffeSettings{
	TrustDomain:      "nico.local",
	ServiceBasePaths: []string{"/ns/forge/sa/"},
	MachineBasePath:  "/machine/",
}

func TestCheckIdentity(t *testing.T) {
	for _, tc := range []struct {
		name, uri, wantID, wantErr string
	}{
		{"the write service", "spiffe://nico.local/ns/forge/sa/machine-a-tron", "spiffe://nico.local/ns/forge/sa/machine-a-tron", ""},
		{"another service", "spiffe://nico.local/ns/forge/sa/other", "", "the SPIFFE ID names another service, not machine-a-tron"},
		{"a machine", "spiffe://nico.local/machine/host-SECRET", "", "the SPIFFE ID names a machine, not service machine-a-tron"},
		{"another trust domain", "spiffe://evil.example/ns/forge/sa/machine-a-tron", "", "the SPIFFE ID is not in the configured trust domain"},
		{"no recognized prefix", "spiffe://nico.local/unknown/SECRET", "", "the SPIFFE ID names no service or machine under the configured base paths"},
		{"a trailing slash path", "spiffe://nico.local/ns/forge/sa/", "", "the SPIFFE ID path is not valid"},
		// A malformed ID is refused by its raw rule, never normalized into a valid service. None of the reasons repeats the ID.
		{"userinfo", "spiffe://u@nico.local/ns/forge/sa/machine-a-tron", "", "the SPIFFE ID carries userinfo"},
		{"a query", "spiffe://nico.local/ns/forge/sa/machine-a-tron?SECRET=1", "", "the SPIFFE ID has a query"},
		{"a bare question mark", "spiffe://nico.local/ns/forge/sa/machine-a-tron?", "", "the SPIFFE ID has a query"},
		{"a fragment", "spiffe://nico.local/ns/forge/sa/machine-a-tron#SECRET", "", "the SPIFFE ID has a fragment"},
		{"an escaped path", "spiffe://nico.local/ns/forge/sa/machine%2Da%2Dtron", "", "the SPIFFE ID path is not valid"},
		{"a dot segment", "spiffe://nico.local/ns/forge/sa/../SECRET", "", "the SPIFFE ID path is not valid"},
		{"a port", "spiffe://nico.local:8443/ns/forge/sa/machine-a-tron", "", "the SPIFFE ID trust domain is missing or invalid"},
	} {
		certPEM, _, _ := leaf(t, tc.uri, nil)
		id, err := CheckIdentity(certPEM, settings)
		got := ""
		if err != nil {
			got = err.Error()
		}
		is(t, id, tc.wantID, tc.name+" id")
		is(t, got, tc.wantErr, tc.name+" err")
		if err != nil && strings.Contains(err.Error(), "SECRET") {
			t.Errorf("%s: the error repeated the SPIFFE ID: %q", tc.name, err)
		}
	}
}

func TestCheckIdentityCertRules(t *testing.T) {
	good := "spiffe://nico.local/ns/forge/sa/machine-a-tron"
	for _, tc := range []struct {
		name, want string
		edit       func(*x509.Certificate)
		uri        string
	}{
		{"a CA is refused", "the certificate is a CA, not a leaf", func(c *x509.Certificate) { c.IsCA = true; c.BasicConstraintsValid = true }, good},
		{"a cert-signing key is refused", "the certificate may sign certificates", func(c *x509.Certificate) { c.KeyUsage |= x509.KeyUsageCertSign }, good},
		{"two URI SANs are refused", "the certificate must have exactly one URI SAN, it has 2", func(c *x509.Certificate) {
			u, _ := url.Parse("spiffe://nico.local/ns/forge/sa/extra")
			c.URIs = append(c.URIs, u)
		}, good},
		{"no URI SAN is refused", "the certificate must have exactly one URI SAN, it has 0", nil, ""},
	} {
		certPEM, _, _ := leaf(t, tc.uri, tc.edit)
		_, err := CheckIdentity(certPEM, settings)
		is(t, err.Error(), tc.want, tc.name)
	}
	_, err := CheckIdentity([]byte("not pem"), settings)
	is(t, err.Error(), "holds no certificate", "no cert")
}

func TestTrustDomainForms(t *testing.T) {
	certPEM, _, _ := leaf(t, "spiffe://nico.local/ns/forge/sa/machine-a-tron", nil)
	for _, tc := range []struct {
		name, td, wantErr string
	}{
		{"a bare name", "nico.local", ""},
		{"a spiffe URL form", "spiffe://nico.local", ""},
		{"not ascii", "niço.local", "the configured trust domain is not ASCII"},
		{"a bad character", "Nico.Local", "the configured trust domain is not valid: a disallowed character"},
		{"empty", "", "the configured trust domain is not valid: it is empty"},
	} {
		s := settings
		s.TrustDomain = tc.td
		_, err := CheckIdentity(certPEM, s)
		got := ""
		if err != nil {
			got = err.Error()
		}
		is(t, got, tc.wantErr, tc.name)
	}
}

func TestCheckKeyPair(t *testing.T) {
	certPEM, keyPEM, _ := leaf(t, "spiffe://nico.local/ns/forge/sa/machine-a-tron", nil)
	_, otherKeyPEM, _ := leaf(t, "spiffe://nico.local/ns/forge/sa/machine-a-tron", nil)
	is(t, errText(CheckKeyPair(certPEM, keyPEM)), "", "the matching pair")
	is(t, errText(CheckKeyPair(certPEM, otherKeyPEM)), "the key is not the certificate's", "a foreign key")
	is(t, errText(CheckKeyPair(certPEM, nil)), "the key file does not start with a private key", "no key")
	is(t, errText(CheckKeyPair(certPEM, certPEM)), "the key file does not start with a private key", "a cert as a key")
}

func TestDialRefusals(t *testing.T) {
	dir := t.TempDir()
	certPEM, keyPEM, _ := leaf(t, "spiffe://nico.local/ns/forge/sa/machine-a-tron", nil)
	write := func(name string, data []byte) { _ = os.WriteFile(filepath.Join(dir, name), data, 0o600) }
	write("tls.crt", certPEM)
	write("tls.key", keyPEM)
	write("ca.crt", certPEM)
	s := Settings{CertDir: dir, Spiffe: settings, ServerName: "forge.local"}

	_, err := Dial("http://nico", s)
	is(t, errText(err), "the nico-api URL is not https", "bad url")

	t.Setenv(tlsOverride, "1")
	_, err = Dial("https://nico", s)
	is(t, strings.Contains(errText(err), tlsOverride+" is set"), true, "override")
	_ = os.Unsetenv(tlsOverride)

	// A key that no longer matches the certificate fails before any dial.
	_, foreignKey, _ := leaf(t, "spiffe://nico.local/ns/forge/sa/machine-a-tron", nil)
	write("tls.key", foreignKey)
	_, err = Dial("https://nico", s)
	is(t, strings.HasSuffix(errText(err), "the key is not the certificate's"), true, "foreign key")

	write("tls.key", keyPEM)
	c, err := Dial("https://nico:443", s)
	is(t, errText(err), "", "dial builds")
	if c != nil {
		_ = c.Close()
	}
}

func errText(err error) string {
	if err == nil {
		return ""
	}
	return err.Error()
}
