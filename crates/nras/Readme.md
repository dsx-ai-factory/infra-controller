# NVIDIA Remote Attestation Service client

The client submits one or more labelled SPDM measurement transcripts as CMW JSON
to `/v1/verify`. It returns one unvalidated EAR JWT; call
`VerifierResponseJwt::validate_and_parse` with the service's signing-key store
before using its claims. HTTP 200 and a valid signature do not establish a
successful appraisal. The SPDM controller accepts only `affirming` overall and
device verdicts; `warning` and `contraindicated` fail attestation.

For a negative appraisal, the controller stores a failure message containing
the device label, overall and device verdicts, any appraisal error details, and
the result identifier (`jti`). If the response omits error details, the message
states that no appraisal details were provided.

`Config::nras_url` is the service base URL, without `/v1/verify`.
`nras_jwks_url` is the full signing-key URL, normally
`<service-base>/.well-known/jwks.json`. `validate_jwt_expiry` controls expiry
validation; `Config::default()` enables it. All three fields are required when
deserialising configuration. Signature and not-before checks always apply.
Time validation allows 60 seconds of clock skew. Remove `nras_gpu_url_suffix`
from existing configuration: the verify path is fixed, and unknown settings are
rejected.

The client accepts standard Base64 evidence and PEM-chain encodings and a
hexadecimal nonce, then converts them to unpadded Base64url CMW records. The nonce
must contain 32–96 decoded bytes and match the nonce signed into that evidence.
Device labels must be unique and must not be `__cmwc_t`, `nonce` or
`appraisal-settings`. An empty collection or malformed encoding fails before the
HTTP request. Non-200 responses return a communication error containing the
status, without including the response body.

The GPU example requires real evidence, its matching nonce and a PEM certificate
chain ordered from the leaf certificate through L1. It prints validated claims,
including negative verdicts. To inspect its arguments from the repository root:

```bash
cargo run -p nras --example nras_gpu -- --help
```

Set `NRAS_EVIDENCE`, `NRAS_NONCE` and `NRAS_CERTIFICATE_PATH` to supply those
required inputs through the environment. The example defaults to
`https://nras.attestation.nvidia.com`, derives the signing-key URL from that base,
and uses `gpu_0` as its device label. Override these with `--nras-url`,
`--nras-jwks-url` and `--device-id`, or the corresponding `NRAS_URL`,
`NRAS_JWKS_URL` and `NRAS_DEVICE_ID` variables; command-line values take precedence.

This client submits SPDM evidence with an explicit certificate chain. It omits
optional request-level nonce, appraisal settings and hints records. Inline
certificate chains, other evidence media types and bearer authentication are not
exposed by this interface. The shared verify method does not enable additional
device types in the SPDM controller.
