# Issue tracker: GitHub

Issues and PRDs for this repository live in GitHub Issues at `proerror77/monday`. Use the `gh` CLI for issue operations and infer the repository from the configured remote.

## Conventions

- Create: `gh issue create --title "..." --body "..."`
- Read: `gh issue view <number> --comments`
- List: `gh issue list --state open --json number,title,body,labels,comments`
- Comment: `gh issue comment <number> --body "..."`
- Label: `gh issue edit <number> --add-label "..."`
- Close: `gh issue close <number> --comment "..."`

## Pull requests as a triage surface

External pull requests are not treated as feature requests by the triage workflow.

## Skill routing

When a skill says to publish to the issue tracker, create a GitHub issue. When it says to fetch a ticket, use `gh issue view` and include comments and labels.
