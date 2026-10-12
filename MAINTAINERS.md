# NVIDIA Infra Controller Maintainers

This document identifies the durable maintainer roles for NVIDIA Infra
Controller (NICo), their ownership areas, and the process for joining or
leaving the maintainer group. GitHub team membership supplies the current
individual membership for team-based roles.

## Current Maintainer Roles

| Role or ownership area | Current maintainer identity | Responsibilities |
| --- | --- | --- |
| Project governance and repository administration | `@dsx-ai-factory/carbide-sw-admins` | Stage 3 project-lead function, repository administration, governance decisions, and conduct appeals |
| General code review and merge approval | `@dsx-ai-factory/carbide-sw-approvers` | Cross-project code review, merge decisions, issue triage, and release participation |
| CI/CD | `@dsx-ai-factory/dsx-sw-cicd` | Workflows, build infrastructure, required checks, and release automation |
| Helm and deployment prerequisites | `@dsx-ai-factory/dsx-sw-helm` | Helm charts, deployment prerequisites, and Kubernetes packaging |
| Documentation | `@polarweasel`, `@CoCo-Ben` | Operator and developer documentation, navigation, and publication review |

The repository's `.github/CODEOWNERS` file must remain consistent with these
ownership areas. Access to a GitHub team does not by itself excuse a maintainer
from the responsibilities or conduct standards in this document.

Team-based maintainer membership must be publicly visible on GitHub. If a team
is not publicly visible, the administrative maintainers must list its active
individual GitHub handles in this document so contributors can identify who is
accountable for reviews, governance decisions, and conduct enforcement.

## Maintainer Responsibilities

Maintainers are expected to:

- Review and merge pull requests in their ownership areas.
- Triage incoming issues and route questions to the appropriate public channel.
- Give external and internal contributors the same requirements and quality bar.
- Explain rejection or requested changes in public, actionable terms.
- Participate in release planning and execution when their area is affected.
- Protect confidential security, conduct, legal, and personnel information.
- Enforce [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) using its documented process.
- Disclose conflicts of interest and recuse when impartiality could reasonably
  be questioned.

## Review Service Levels

These targets begin when a pull request is ready for review and required tests
are available. They are service objectives, not automatic acceptance promises.

| Pull request type | First response | Target decision |
| --- | --- | --- |
| Bug fix | Three business days | Ten business days |
| Feature | Five business days | Twenty-one business days |
| Coordinated security fix | One business day | Three business days |

When a target cannot be met, the assigned maintainer should post a status update
and identify the next action or missing dependency. Security vulnerabilities
must first be reported privately as described in [SECURITY.md](SECURITY.md).

## Becoming a Maintainer

Maintainer candidates may be NVIDIA employees or external contributors. The
normal path is:

1. Sustain high-quality contributions for at least three months.
2. Demonstrate understanding of the relevant subsystem and project direction.
3. Demonstrate constructive reviews and reliable community participation.
4. Receive nomination from an existing maintainer.
5. Complete a public five-business-day comment period with no unresolved
   maintainer objection.
6. Receive confirmation from the Stage 3 project-lead function.

The nomination must identify the candidate's ownership area. After approval,
the administrative maintainers update this file and CODEOWNERS together.

## Stepping Down and Emeritus Status

A maintainer may step down by notifying the maintainer group. A maintainer who
has been unresponsive in the project for 90 days may be moved to emeritus status
after the maintainers attempt contact and the project-lead function confirms
the change.

Emeritus maintainers retain recognition for past contributions but no longer
hold merge, governance, or conduct-enforcement authority. They may return to
active status through the maintainer appointment process, with prior service
considered as evidence of experience.

## Removal

A maintainer may be removed for sustained failure to perform the role, repeated
violation of project policy, a serious Code of Conduct violation, misuse of
repository access, or a material loss of community trust.

Except for an urgent access suspension needed to protect the project:

1. The concern and supporting facts are provided privately to the maintainer.
2. Involved or conflicted decision-makers recuse themselves.
3. The maintainer has a reasonable opportunity to respond.
4. The project-lead function consults at least one uninvolved maintainer and
   makes the Stage 3 decision.
5. Repository access and this file are updated together.

The project records the role change publicly. Confidential conduct, security,
legal, or personnel details remain private. The affected maintainer may request
a secondary review by an administrative maintainer who did not participate in
the original decision.

## Governance

Decision authority, proposal requirements, pull request appeals, and conflicts
of interest are defined in [GOVERNANCE.md](GOVERNANCE.md).
