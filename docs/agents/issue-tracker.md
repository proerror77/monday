# Issue tracker: GitHub

Issues and PRDs for this repo live as GitHub issues. Use the `gh` CLI and infer the repository from `git remote -v`.

## Operations

- Create: `gh issue create --title "..." --body "..."`
- Read: `gh issue view <number> --comments`
- List: `gh issue list --state open --json number,title,body,labels,comments`
- Comment: `gh issue comment <number> --body "..."`
- Label: `gh issue edit <number> --add-label "..."` or `--remove-label "..."`
- Close: `gh issue close <number> --comment "..."`

## Pull requests as a triage surface

PRs as a request surface: no.

When a skill says to publish to the issue tracker, create a GitHub issue. When it says to fetch a ticket, use `gh issue view <number> --comments`.
