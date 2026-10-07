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

//! TPM related operations

use std::process::Command;

use x509_parser::prelude::{FromDer, X509Certificate};

const TPM2_GET_EK_CERTIFICATE: &str = "tpm2_getekcertificate";
const TPM2_NV_READ: &str = "tpm2_nvread";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EkKeyType {
    Rsa,
    Ecc,
}

#[derive(Clone, Copy, Debug)]
struct EkCertNvIndex {
    name: &'static str,
    key_type: EkKeyType,
    index: &'static str,
}

// Keep this in the same order as the tpm2_getekcertificate versions deployed in
// the runtime image: standard RSA first, then standard ECC.
const TPM_EK_CERT_NV_INDICES: &[EkCertNvIndex] = &[
    EkCertNvIndex {
        name: "rsa",
        key_type: EkKeyType::Rsa,
        index: "0x01c00002",
    },
    EkCertNvIndex {
        name: "ecc",
        key_type: EkKeyType::Ecc,
        index: "0x01c0000a",
    },
];

/// Enumerates errors for TPM related operations
#[derive(Debug, thiserror::Error)]
pub(super) enum TpmError {
    #[error("unable to invoke subprocess {0}: {1}")]
    Subprocess(&'static str, std::io::Error),
    #[error("subprocess exited with exit code {0:?}. stderr: {1}")]
    SubprocessStatusNotOk(Option<i32>, String),
    #[error("TPM EK certificate bytes from {0} were not parseable as DER X.509")]
    InvalidEkCertificate(&'static str),
    #[error("unable to read TPM EK certificate: {primary_error}; NV fallback errors: {nv_errors}")]
    EkCertificateNotFound {
        primary_error: Box<TpmError>,
        nv_errors: String,
    },
}

/// Returns the TPM's endorsement key certificate in binary format
pub(super) fn get_ek_certificate() -> Result<Vec<u8>, TpmError> {
    get_ek_certificate_with_runner(&StdCommandRunner)
}

pub(super) fn is_tpm_present() -> bool {
    std::path::Path::new("/dev/tpmrm0").exists() || std::path::Path::new("/dev/tpm0").exists()
}

#[derive(Debug)]
struct CommandOutput {
    status_success: bool,
    status_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

trait CommandRunner {
    fn output(&self, program: &'static str, args: &[&str])
    -> Result<CommandOutput, std::io::Error>;
}

struct StdCommandRunner;

impl CommandRunner for StdCommandRunner {
    fn output(
        &self,
        program: &'static str,
        args: &[&str],
    ) -> Result<CommandOutput, std::io::Error> {
        let output = Command::new(program).args(args).output()?;

        Ok(CommandOutput {
            status_success: output.status.success(),
            status_code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

fn get_ek_certificate_with_runner(runner: &impl CommandRunner) -> Result<Vec<u8>, TpmError> {
    match get_ek_certificate_from_tool(runner) {
        Ok(cert) => Ok(cert),
        Err(primary_error) => {
            tracing::warn!(
                command = TPM2_GET_EK_CERTIFICATE,
                error = ?primary_error,
                "Could not read TPM EK certificate; probing known NV indices"
            );
            let mut certs = vec![];
            let mut nv_errors = vec![];
            for nv_index in TPM_EK_CERT_NV_INDICES {
                match get_ek_certificate_from_nv_index(runner, nv_index.index) {
                    Ok(cert) => {
                        tracing::info!(
                            name = nv_index.name,
                            index = nv_index.index,
                            "Read TPM EK certificate from NV index"
                        );
                        certs.push((*nv_index, cert));
                    }
                    Err(e) => nv_errors.push(format!("{}: {e}", nv_index.index)),
                }
            }

            if let Some(cert) = select_tool_compatible_nv_ek_certificates(certs) {
                X509Certificate::from_der(&cert)
                    .map_err(|_| TpmError::InvalidEkCertificate(TPM2_NV_READ))?;
                return Ok(cert);
            }

            Err(TpmError::EkCertificateNotFound {
                primary_error: Box::new(primary_error),
                nv_errors: nv_errors.join("; "),
            })
        }
    }
}

fn select_tool_compatible_nv_ek_certificates(
    certs: Vec<(EkCertNvIndex, Vec<u8>)>,
) -> Option<Vec<u8>> {
    let mut rsa_cert = None;
    let mut ecc_cert = None;

    for (nv_index, cert) in certs {
        // Match the tpm2_getekcertificate stdout shape NICo already hashes:
        // one RSA EK certificate followed by one ECC EK certificate. Extra
        // readable same-family NV indices must not change the identity bytes.
        match nv_index.key_type {
            EkKeyType::Rsa if rsa_cert.is_none() => rsa_cert = Some(cert),
            EkKeyType::Ecc if ecc_cert.is_none() => ecc_cert = Some(cert),
            _ => {}
        }
    }

    match (rsa_cert, ecc_cert) {
        (Some(mut rsa_cert), Some(ecc_cert)) => {
            rsa_cert.extend_from_slice(&ecc_cert);
            Some(rsa_cert)
        }
        (Some(rsa_cert), None) => Some(rsa_cert),
        (None, Some(ecc_cert)) => Some(ecc_cert),
        (None, None) => None,
    }
}

fn get_ek_certificate_from_tool(runner: &impl CommandRunner) -> Result<Vec<u8>, TpmError> {
    // TODO: Do we need the `--raw` or `--offline` parameters?
    let output = runner
        .output(TPM2_GET_EK_CERTIFICATE, &[])
        .map_err(|e| TpmError::Subprocess(TPM2_GET_EK_CERTIFICATE, e))?;

    cert_from_output(TPM2_GET_EK_CERTIFICATE, output)
}

fn get_ek_certificate_from_nv_index(
    runner: &impl CommandRunner,
    index: &str,
) -> Result<Vec<u8>, TpmError> {
    let output = runner
        .output(TPM2_NV_READ, &["-C", "o", index])
        .map_err(|e| TpmError::Subprocess(TPM2_NV_READ, e))?;

    nv_output(output)
}

fn cert_from_output(source: &'static str, output: CommandOutput) -> Result<Vec<u8>, TpmError> {
    let stdout = checked_stdout(output)?;

    X509Certificate::from_der(&stdout).map_err(|_| TpmError::InvalidEkCertificate(source))?;

    Ok(stdout)
}

fn nv_output(output: CommandOutput) -> Result<Vec<u8>, TpmError> {
    checked_stdout(output)
}

fn checked_stdout(output: CommandOutput) -> Result<Vec<u8>, TpmError> {
    if !output.status_success {
        let err = String::from_utf8(output.stderr).unwrap_or_else(|_| "Invalid UTF8".to_string());
        return Err(TpmError::SubprocessStatusNotOk(output.status_code, err));
    }

    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};
    use rcgen::{CertifiedKey, generate_simple_self_signed};

    use super::*;

    #[derive(Debug)]
    struct FakeCall {
        program: &'static str,
        args: Vec<&'static str>,
        result: Result<CommandOutput, io::Error>,
    }

    #[derive(Debug)]
    struct FakeRunner {
        calls: std::cell::RefCell<VecDeque<FakeCall>>,
    }

    impl FakeRunner {
        fn new(calls: Vec<FakeCall>) -> Self {
            Self {
                calls: std::cell::RefCell::new(calls.into()),
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn output(
            &self,
            program: &'static str,
            args: &[&str],
        ) -> Result<CommandOutput, std::io::Error> {
            let call = self
                .calls
                .borrow_mut()
                .pop_front()
                .expect("unexpected call");
            assert_eq!(call.program, program);
            assert_eq!(call.args, args);
            call.result
        }
    }

    fn test_ek_cert_der(common_name: &str) -> Vec<u8> {
        let CertifiedKey { cert, .. } =
            generate_simple_self_signed(vec![common_name.to_string()]).unwrap();
        cert.der().to_vec()
    }

    fn successful_output(stdout: &[u8]) -> CommandOutput {
        CommandOutput {
            status_success: true,
            status_code: Some(0),
            stdout: stdout.to_vec(),
            stderr: vec![],
        }
    }

    fn failed_output(stderr: &str) -> CommandOutput {
        CommandOutput {
            status_success: false,
            status_code: Some(2),
            stdout: vec![],
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    fn cert_with_trailing_nv_bytes(cert: &[u8]) -> Vec<u8> {
        let mut stdout = cert.to_vec();
        stdout.extend_from_slice(b"unspecified NV bytes");
        stdout
    }

    fn primary_tool_failed_call() -> FakeCall {
        FakeCall {
            program: TPM2_GET_EK_CERTIFICATE,
            args: vec![],
            result: Ok(failed_output(
                "ERROR: Must specify the EK public key path\n",
            )),
        }
    }

    fn nv_read_call(index: &'static str, result: Result<CommandOutput, io::Error>) -> FakeCall {
        FakeCall {
            program: TPM2_NV_READ,
            args: vec!["-C", "o", index],
            result,
        }
    }

    fn failed_nv_read_call(index: &'static str) -> FakeCall {
        nv_read_call(index, Ok(failed_output("NV index not available")))
    }

    /// All NV indices after `first_n` fail; helper to build the tail of a
    /// fallback call sequence.
    fn failing_nv_tail(first_n: usize) -> Vec<FakeCall> {
        TPM_EK_CERT_NV_INDICES[first_n..]
            .iter()
            .map(|nv_index| failed_nv_read_call(nv_index.index))
            .collect()
    }

    #[test]
    fn nv_fallback_matches_normal_tool_rsa_ecc_stdout_bytes() {
        let rsa_cert = test_ek_cert_der("rsa");
        let ecc_cert = test_ek_cert_der("ecc");
        let rsa_nv = cert_with_trailing_nv_bytes(&rsa_cert);
        let ecc_nv = cert_with_trailing_nv_bytes(&ecc_cert);
        let mut normal_tool_stdout = rsa_nv.clone();
        normal_tool_stdout.extend_from_slice(&ecc_nv);

        let normal_runner = FakeRunner::new(vec![FakeCall {
            program: TPM2_GET_EK_CERTIFICATE,
            args: vec![],
            result: Ok(successful_output(&normal_tool_stdout)),
        }]);
        let normal_cert = get_ek_certificate_with_runner(&normal_runner).unwrap();
        assert!(
            normal_runner.calls.borrow().is_empty(),
            "normal runner not drained"
        );

        let fallback_runner = FakeRunner::new(vec![
            primary_tool_failed_call(),
            nv_read_call(
                TPM_EK_CERT_NV_INDICES[0].index,
                Ok(successful_output(&rsa_nv)),
            ),
            nv_read_call(
                TPM_EK_CERT_NV_INDICES[1].index,
                Ok(successful_output(&ecc_nv)),
            ),
        ]);
        let fallback_cert = get_ek_certificate_with_runner(&fallback_runner).unwrap();
        assert!(
            fallback_runner.calls.borrow().is_empty(),
            "fallback runner not drained"
        );

        assert_eq!(normal_cert, normal_tool_stdout);
        assert_eq!(fallback_cert, normal_cert);
    }

    /// `get_ek_certificate_with_runner`: primary-tool path, NV fallback,
    /// invalid-NV skipping, tool-compatible selection, plus the no-cert-anywhere
    /// failure path. The runner is consumed in full
    /// (asserted empty) by `FakeRunner`'s `expect("unexpected call")`. Error type
    /// (`TpmError`) is not `PartialEq`, so failures use `Fails`.
    #[test]
    fn get_ek_certificate_with_runner_cases() {
        // Built lazily inside each input closure because the expected `Vec<u8>`
        // and the runner's fake certs must be derived from the same DER bytes.
        let primary_cert = test_ek_cert_der("primary");
        let fallback_cert = test_ek_cert_der("fallback");
        let first_cert = test_ek_cert_der("first");
        let ecc_cert = test_ek_cert_der("ecc");
        let fallback_nv = cert_with_trailing_nv_bytes(&fallback_cert);
        let first_nv = cert_with_trailing_nv_bytes(&first_cert);
        let ecc_nv = cert_with_trailing_nv_bytes(&ecc_cert);
        let ecc_nv_for_nonstandard = ecc_nv.clone();
        let mut first_ecc_nv = first_nv.clone();
        first_ecc_nv.extend_from_slice(&ecc_nv);
        let mut first_malformed_ecc_nv = first_nv.clone();
        first_malformed_ecc_nv.extend_from_slice(b"not a certificate");

        // Input is a (runner, expected-bytes-if-any) pair; the closure runs the
        // runner and returns the cert vec, dropping the non-PartialEq error.
        scenarios!(
            run = |runner| {
                let cert = get_ek_certificate_with_runner(&runner).map_err(drop)?;
                assert!(runner.calls.borrow().is_empty(), "runner not drained");
                Ok::<_, ()>(cert)
            };
            "primary tool returns the certificate; no NV probing" {
                FakeRunner::new(vec![FakeCall {
                    program: TPM2_GET_EK_CERTIFICATE,
                    args: vec![],
                    result: Ok(successful_output(&primary_cert)),
                }]) => Yields(primary_cert),
            }

            "primary fails, falls back to first NV index" {
                {
                    let mut calls = vec![
                        primary_tool_failed_call(),
                        nv_read_call(
                            TPM_EK_CERT_NV_INDICES[0].index,
                            Ok(successful_output(&fallback_nv)),
                        ),
                    ];
                    calls.extend(failing_nv_tail(1));
                    FakeRunner::new(calls)
                } => Yields(fallback_nv),
            }

            "invalid leading RSA bytes fail even when ECC bytes are valid" {
                {
                    let mut calls = vec![
                        primary_tool_failed_call(),
                        nv_read_call(
                            TPM_EK_CERT_NV_INDICES[0].index,
                            Ok(successful_output(b"not a certificate")),
                        ),
                        nv_read_call(
                            TPM_EK_CERT_NV_INDICES[1].index,
                            Ok(successful_output(&ecc_nv)),
                        ),
                    ];
                    calls.extend(failing_nv_tail(2));
                    FakeRunner::new(calls)
                } => Fails,
            }

            "fallback matches normal path with concatenated RSA and ECC cert output" {
                {
                    FakeRunner::new(vec![
                        primary_tool_failed_call(),
                        nv_read_call(
                            TPM_EK_CERT_NV_INDICES[0].index,
                            Ok(successful_output(&first_nv)),
                        ),
                        nv_read_call(
                            TPM_EK_CERT_NV_INDICES[1].index,
                            Ok(successful_output(&ecc_nv)),
                        ),
                    ])
                } => Yields(first_ecc_nv),
            }

            "malformed ECC bytes are retained after valid RSA bytes" {
                {
                    FakeRunner::new(vec![
                        primary_tool_failed_call(),
                        nv_read_call(
                            TPM_EK_CERT_NV_INDICES[0].index,
                            Ok(successful_output(&first_nv)),
                        ),
                        nv_read_call(
                            TPM_EK_CERT_NV_INDICES[1].index,
                            Ok(successful_output(b"not a certificate")),
                        ),
                    ])
                } => Yields(first_malformed_ecc_nv),
            }

            "falls back to first ECC cert when no RSA cert is readable" {
                {
                    let mut calls = vec![
                        primary_tool_failed_call(),
                        failed_nv_read_call(TPM_EK_CERT_NV_INDICES[0].index),
                        nv_read_call(
                            TPM_EK_CERT_NV_INDICES[1].index,
                            Ok(successful_output(&ecc_nv)),
                        ),
                    ];
                    calls.extend(failing_nv_tail(2));
                    FakeRunner::new(calls)
                } => Yields(ecc_nv),
            }

            "non-standard RSA indices are not probed when standard RSA is missing" {
                {
                    let mut calls = vec![
                        primary_tool_failed_call(),
                        failed_nv_read_call(TPM_EK_CERT_NV_INDICES[0].index),
                        nv_read_call(
                            TPM_EK_CERT_NV_INDICES[1].index,
                            Ok(successful_output(&ecc_nv_for_nonstandard)),
                        ),
                    ];
                    calls.extend(failing_nv_tail(2));
                    FakeRunner::new(calls)
                } => Yields(ecc_nv_for_nonstandard),
            }

            "primary fails and every NV index fails: no cert found" {
                {
                    let mut calls = vec![primary_tool_failed_call()];
                    calls.extend(failing_nv_tail(0));
                    FakeRunner::new(calls)
                } => Fails,
            }

            "primary subprocess spawn errors, every NV index fails" {
                {
                    let mut calls = vec![FakeCall {
                        program: TPM2_GET_EK_CERTIFICATE,
                        args: vec![],
                        result: Err(io::Error::new(io::ErrorKind::NotFound, "missing")),
                    }];
                    calls.extend(failing_nv_tail(0));
                    FakeRunner::new(calls)
                } => Fails,
            }
        );
    }

    /// `checked_stdout`: success passes stdout through unchanged; any non-success
    /// status is an error regardless of code or stderr validity. Pure over a
    /// `CommandOutput`. Error type is not `PartialEq`, so failures use `Fails`.
    #[test]
    fn checked_stdout_cases() {
        scenarios!(
            run = |output| checked_stdout(output).map_err(drop);
            "success returns stdout verbatim" {
                CommandOutput {
                    status_success: true,
                    status_code: Some(0),
                    stdout: b"payload".to_vec(),
                    stderr: vec![],
                } => Yields(b"payload".to_vec()),
            }

            "success with empty stdout returns empty" {
                CommandOutput {
                    status_success: true,
                    status_code: Some(0),
                    stdout: vec![],
                    stderr: b"warning".to_vec(),
                } => Yields(vec![]),
            }

            "non-success with utf8 stderr fails" {
                failed_output("boom") => Fails,
            }

            "non-success with no status code fails" {
                CommandOutput {
                    status_success: false,
                    status_code: None,
                    stdout: vec![],
                    stderr: vec![],
                } => Fails,
            }

            "non-success with invalid utf8 stderr still fails (no panic)" {
                CommandOutput {
                    status_success: false,
                    status_code: Some(1),
                    stdout: vec![],
                    stderr: vec![0xff, 0xfe],
                } => Fails,
            }
        );
    }

    /// `cert_from_output`: requires a valid DER X.509 in stdout and returns the
    /// full stdout buffer. Non-success status and unparseable bytes both fail.
    #[test]
    fn cert_from_output_cases() {
        let cert = test_ek_cert_der("tool");
        let cert_with_trailing = cert_with_trailing_nv_bytes(&cert);

        scenarios!(
            run = |output| cert_from_output(TPM2_GET_EK_CERTIFICATE, output).map_err(drop);
            "valid DER returns the full stdout" {
                successful_output(&cert) => Yields(cert),
            }

            "trailing bytes are NOT trimmed (full stdout returned)" {
                successful_output(&cert_with_trailing) => Yields(cert_with_trailing),
            }

            "non-certificate stdout fails to parse" {
                successful_output(b"not a certificate") => Fails,
            }

            "empty stdout fails to parse" {
                successful_output(b"") => Fails,
            }

            "subprocess non-success fails before parsing" {
                failed_output("tool error") => Fails,
            }
        );
    }

    /// `nv_output`: returns successful stdout unchanged, including any trailing
    /// padding or malformed bytes. Fallback validates only after selected NV
    /// buffers are concatenated to match the normal tool path.
    #[test]
    fn nv_output_cases() {
        let cert = test_ek_cert_der("nv");
        let cert_with_trailing = cert_with_trailing_nv_bytes(&cert);

        scenarios!(
            run = |output| nv_output(output).map_err(drop);
            "exact-length DER returns it unchanged" {
                successful_output(&cert) => Yields(cert),
            }

            "trailing NV padding is retained" {
                successful_output(&cert_with_trailing) => Yields(cert_with_trailing),
            }

            "non-certificate stdout is retained" {
                successful_output(b"not a certificate") => Yields(b"not a certificate".to_vec()),
            }

            "empty stdout is retained" {
                successful_output(b"") => Yields(vec![]),
            }

            "subprocess non-success fails before parsing" {
                failed_output("nv error") => Fails,
            }
        );
    }

    /// `TpmError` Display formatting: each variant's message must mention its
    /// salient tokens. Total over the error value, so `check_values` with a
    /// token-contains predicate.
    #[test]
    fn tpm_error_display_contains_tokens() {
        value_scenarios!(
            run = |(error, tokens)| {
                let rendered = error.to_string();
                tokens.iter().all(|t| rendered.contains(t))
            };
            "Subprocess names the program" {
                (
                    TpmError::Subprocess(
                        TPM2_NV_READ,
                        io::Error::new(io::ErrorKind::NotFound, "x"),
                    ),
                    &[TPM2_NV_READ, "unable to invoke"][..],
                ) => true,
            }

            "SubprocessStatusNotOk names the stderr" {
                (
                    TpmError::SubprocessStatusNotOk(Some(2), "boom".to_string()),
                    &["boom", "exit code"][..],
                ) => true,
            }

            "InvalidEkCertificate names the source" {
                (
                    TpmError::InvalidEkCertificate(TPM2_GET_EK_CERTIFICATE),
                    &[TPM2_GET_EK_CERTIFICATE, "DER X.509"][..],
                ) => true,
            }

            "EkCertificateNotFound names primary and nv errors" {
                (
                    TpmError::EkCertificateNotFound {
                        primary_error: Box::new(TpmError::InvalidEkCertificate(
                            TPM2_GET_EK_CERTIFICATE,
                        )),
                        nv_errors: "0x01c00002: nope".to_string(),
                    },
                    &["NV fallback errors", "0x01c00002: nope"][..],
                ) => true,
            }
        );
    }
}
