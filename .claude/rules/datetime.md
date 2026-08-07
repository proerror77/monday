# DateTime Rule

## Getting Current Date and Time

When any command requires the current date/time (for frontmatter, timestamps, or logs), you MUST obtain the REAL current date/time from the system rather than estimating or using placeholder values.

### How to Get Current DateTime

Use the `date` command to get the current ISO 8601 formatted datetime:

```bash
# Get current datetime in ISO 8601 format (works on Linux/Mac)
date -u +"%Y-%m-%dT%H:%M:%SZ"

# Alternative for systems that support it
date --iso-8601=seconds

# For Windows (if using PowerShell)
Get-Date -Format "yyyy-MM-ddTHH:mm:ssZ"
```

### Required Format

All dates in frontmatter MUST use ISO 8601 format with UTC timezone:
- Format: `YYYY-MM-DDTHH:MM:SSZ`
- Example: `2024-01-15T14:30:45Z`

### Usage in Frontmatter

When creating or updating frontmatter in any file (PRD, Epic, Task, Progress), always use the real current datetime:

```yaml
---
name: feature-name
created: 2024-01-15T14:30:45Z  # Use actual output from date command
updated: 2024-01-15T14:30:45Z  # Use actual output from date command
---
```

### Important Notes

- **Never use placeholder dates** like `[Current ISO date/time]` or `YYYY-MM-DD`
- **Never estimate dates** - always get the actual system time
- **Always use UTC** (the `Z` suffix) for consistency across timezones
- **Preserve timezone consistency** - all dates in the system use UTC

### Rule Priority

This rule has **HIGHEST PRIORITY** and must be followed by all commands that:
- Create new files with frontmatter
- Update existing files with frontmatter
- Track timestamps or progress
- Log any time-based information

Commands affected: prd-new, prd-parse, epic-decompose, epic-sync, issue-start, issue-sync, and any other command that writes timestamps.
