# NVIDIA Infra Controller Governance

This document defines how NVIDIA Infra Controller (NICo) decisions are made,
recorded, and reviewed under the DSX OS governance model. It applies to
maintainers and contributors regardless of employer.

## Governance Stage

NICo operates at **Stage 3**:

- Development occurs in the public repository.
- External contributors may open issues, Discussions, and pull requests.
- Maintainers review contributions using the same published criteria for
  internal and external contributors.
- NVIDIA retains final authority through the project-lead function described
  below.

The project may move to Stage 4 community governance after it has an active
maintainer group with meaningful external representation and is prepared to
use public lazy consensus for technical and roadmap decisions. A stage change
requires a public proposal and an update to this document.

## Principles

### No blind accountability

Contributors will not be held to processes, tests, or information they cannot
access. When an internal security, compliance, or CI requirement affects a
public contribution, maintainers must translate it into public, actionable
feedback.

### Transparent decisions

Roadmap direction, significant technical decisions, pull request rejection,
and maintainer appointments must be made or recorded in public channels unless
security, privacy, legal, or personnel obligations require confidentiality.

### Consistent treatment

Published contribution and review criteria apply equally to NVIDIA employees
and external contributors.

### Narrow reserved authority

NVIDIA retains authority over intellectual-property decisions, including
license changes, trademarks, project deprecation, and transfer to a foundation.
Security disclosures remain governed by NVIDIA PSIRT and
[SECURITY.md](SECURITY.md).

## Roles and Authority

Current roles and ownership areas are listed in
[MAINTAINERS.md](MAINTAINERS.md).

- The `@dsx-ai-factory/carbide-sw-admins` team performs the Stage 3
  project-lead function. It has final authority over project decisions and
  repository administration after considering maintainer and community input.
- Maintainers review and merge changes in their ownership areas, triage issues,
  participate in planning, and enforce the Code of Conduct.
- Contributors may propose changes, participate in public discussions, review
  work, and request reconsideration of decisions.
- CODEOWNERS routes reviews and protects repository paths. Repository access or
  CODEOWNERS membership alone does not replace the responsibilities and
  accountability documented in `MAINTAINERS.md`.

## Decision Process

| Decision | Process | Final authority at Stage 3 |
| --- | --- | --- |
| Routine implementation or documentation change | Pull request review by the relevant owner | Area maintainer |
| Significant architecture, API, or compatibility change | Public issue or Discussion before implementation; maintainer consultation | Project-lead function |
| Roadmap, release cadence, or contribution-model change | Public proposal with rationale and alternatives | Project-lead function |
| License, trademark, deprecation, or foundation transfer | Public notice when legally possible; NVIDIA review | NVIDIA |
| Security disclosure or embargoed remediation | Private PSIRT process | NVIDIA PSIRT |
| Conduct report | Private Code of Conduct process | Uninvolved conduct reviewers |

A significant proposal should state:

- The problem and affected users.
- The proposed change and its scope.
- Alternatives considered.
- Compatibility, operational, and contributor impact.
- The decision requested and a reasonable comment period.

Maintainers should allow at least five business days for public comment on a
significant proposal unless an urgent security or reliability issue requires a
faster decision. The final decision and rationale must be recorded in the
public thread when confidentiality obligations permit.

## Pull Request Decisions and Appeals

A pull request must not be rejected without a written, actionable explanation.
“Not a fit” is not sufficient. If private information affects the decision, a
maintainer must translate it into the most specific public criterion that can
be shared.

A contributor who believes a pull request was rejected unfairly may:

1. Request a written explanation from the rejecting maintainer.
2. Request review by another maintainer with relevant ownership.
3. Ask the project-lead function for a final Stage 3 decision through a public
   GitHub Discussion.

The final decision and rationale must be recorded publicly unless doing so
would disclose protected information.

## Conflicts of Interest

Maintainers must disclose and recuse themselves from decisions where a personal,
reporting, financial, or other material relationship could reasonably call
their impartiality into question. Another maintainer or an administrative
maintainer must handle the decision.

Conduct matters follow the more specific recusal and confidentiality rules in
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

## Governance Changes

Governance changes require a public proposal and at least five business days
for maintainer and community comment. At Stage 3, the project-lead function
makes the final decision and records the rationale publicly.

## Related Documents

- [Maintainers and ownership](MAINTAINERS.md)
- [Contribution guidelines](CONTRIBUTING.md)
- [Code of Conduct](CODE_OF_CONDUCT.md)
- [Security policy](SECURITY.md)
