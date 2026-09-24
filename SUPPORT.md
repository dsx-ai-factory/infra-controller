# Support

NVIDIA Infra Controller (NICo) is an open-source DSX OS infrastructure
component. This document describes its support paths and release-support
lifecycle.

## Community Support

Use
[GitHub Discussions](https://github.com/dsx-ai-factory/infra-controller/discussions)
for usage questions, deployment guidance, integration help, and contributor
questions. Community support is provided on a best-effort basis and does not
have a service-level agreement.

Use [GitHub Issues](https://github.com/dsx-ai-factory/infra-controller/issues)
for reproducible bug reports and actionable feature requests. Do not include
confidential information, credentials, customer information, or vulnerability
details in an issue.

## Security Support

Report potential security vulnerabilities privately as described in
[`SECURITY.md`](SECURITY.md). Do not use GitHub Issues or Discussions for
security reports.

## Commercial Support

Customers and NVIDIA Cloud Partners with an applicable NVIDIA support agreement
should use the support channel identified in that agreement for production
incidents and contract-governed assistance. Contractual response and resolution
commitments are governed by the applicable agreement, not this document.

## Supported Releases

The [NICo release notes](https://docs.nvidia.com/infra-controller/documentation/release-notes)
are the canonical source for release status:

- **Current** releases receive active fixes and security updates.
- **Maintenance** releases receive maintenance and security updates.
- **EOL** releases no longer receive fixes or security updates.

Operators running an EOL release should upgrade to a current or maintenance
release. Review the release notes and
[upgrade documentation](https://docs.nvidia.com/infra-controller/documentation/operations-day-2/upgrading-nico)
before upgrading production environments.
