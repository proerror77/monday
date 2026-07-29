# Issue tracker: GitHub

Issues and PRDs for this repository live in GitHub Issues at `proerror77/monday`. Use the `gh` CLI for issue operations and infer the repository from the configured remote.

## Conventions

- Create: `gh issue create --title "..." --body-file - --label <category> --label <state>`
- Read: `gh issue view <number> --comments`
- List: `gh issue list --state open --json number,title,body,labels,comments`
- Comment: `gh issue comment <number> --body "..."`
- Label: `gh issue edit <number> --add-label "..."`
- Close: `gh issue close <number> --comment "..."`

Use `--body-file` for multiline Markdown. Do not pass literal `\\n` escapes in
`--body`. After creation, read back the body, labels, and relationships before
claiming publication succeeded.

## Issue contract

- Every issue has exactly one category (`bug` or `enhancement`) and exactly one
  state from `docs/agents/triage-labels.md`.
- `tracking` marks a PRD or parent issue and excludes it from agent pickup.
- `runtime` marks a deployment, cutover, live mutation, or runtime-evidence
  contract. It does not grant authority to perform that mutation.
- Assign exactly one write owner when work starts. The worktree-private
  `agent-worktree.yml` remains the source of branch and file ownership.
- Code behavior, artifact publication, runtime adoption, and result publication
  are separate issues whenever they can be reviewed, reverted, authorized, or
  evidenced independently.

Use GitHub's native parent, sub-issue, and dependency relationships:

```text
gh issue create --parent <parent> --blocked-by <issue_numbers>
gh issue edit <issue> --parent <parent> --add-blocked-by <issue_number>
```

The `Parent` and `Blocked by` body sections remain human-readable summaries;
the native parent and blocked-by relationships are authoritative.

## Pull request relationship

Every PR declares exactly one relationship in its body:

- `Refs #N` for partial work, preparatory work, or a stacked PR whose base is
  not `main`.
- `Closes #N` only when merging the PR into `main` completes the entire code
  contract.
- `None` when no issue applies.

Do not write negated closing phrases such as `does not close #N`; GitHub still
recognizes the closing keyword. The `close`, `fix`, and `resolve` keyword
families in commit messages follow the same restriction because a commit
reaching the default branch can close an issue. A `tracking` or `runtime` issue
must use `Refs`, never a closing keyword. See GitHub's official
[linking contract](https://docs.github.com/en/issues/tracking-your-work-with-issues/using-issues/linking-a-pull-request-to-an-issue).

## Runtime and parent closure

A runtime issue closes manually only after its comment history records the
exact target, named controller, source/candidate and configuration identities,
rollback identity, stop rules, terminal result, and cleanup evidence. A merged
PR or healthy process is not a substitute for that evidence.

When the final direct sub-issue closes, audit the parent outcome and acceptance
criteria. Do not infer parent completion from child state and do not close a
parent automatically. If a different behavior or rollout remains, publish a
new sub-issue rather than extending a completed contract.

## Pull requests as a triage surface

External pull requests are not treated as feature requests by the triage workflow.

## Skill routing

When a skill says to publish to the issue tracker, create a GitHub issue. When it says to fetch a ticket, use `gh issue view` and include comments and labels.
