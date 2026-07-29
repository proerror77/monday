# Triage Labels

Every triaged issue carries exactly one category and one state. Qualifiers add
execution context but never replace either role.

## Category roles

| Skill role | GitHub label | Meaning |
| --- | --- | --- |
| `bug` | `bug` | Existing behavior is broken |
| `enhancement` | `enhancement` | New behavior or an improvement |

## State roles

| Skill role | GitHub label | Meaning |
| --- | --- | --- |
| `needs-triage` | `needs-triage` | Maintainer evaluation is required |
| `needs-info` | `needs-info` | Waiting for reporter information |
| `ready-for-agent` | `ready-for-agent` | Fully specified and safe for an autonomous agent |
| `ready-for-human` | `ready-for-human` | Human implementation or judgment is required |
| `wontfix` | `wontfix` | The issue will not be actioned |

Use the right-hand label verbatim when engineering skills refer to a triage
role. Remove the previous state label when moving an issue; conflicting state
labels are invalid.

## Qualifiers

| GitHub label | Meaning |
| --- | --- |
| `tracking` | A PRD or parent tracker; exclude it from agent pickup queries |
| `runtime` | Closure requires live mutation or runtime evidence |

`ready-for-agent` means the issue is executable now. An issue with missing
authority, target identity, or required input is `needs-info`; an issue whose
next action requires human judgment or control is `ready-for-human`.
