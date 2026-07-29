---
allowed-tools: Bash, Read, Write, LS, Task
---

# Epic Sync

Push epic and tasks to GitHub as issues.

## Usage
```
/pm:epic-sync <feature_name>
```

## Quick Check

```bash
case "$ARGUMENTS" in
  ''|*[!A-Za-z0-9._-]*)
    echo "❌ Epic name must be a simple path-safe slug" >&2
    exit 1
    ;;
esac

epic_dir=".claude/epics/$ARGUMENTS"
if [ ! -f "$epic_dir/epic.md" ]; then
  echo "❌ Epic not found. Run: /pm:prd-parse $ARGUMENTS" >&2
  exit 1
fi

task_source_id() {
  source_id=$(sed -n '2,/^---$/s/^monday_source: *//p' "$1")
  [ -n "$source_id" ] || source_id=$(basename "$1" .md)
  case "$source_id" in
    [0-9][0-9][0-9]) printf '%s\n' "$source_id" ;;
    *) echo "❌ Invalid source task identity in $1" >&2; exit 1 ;;
  esac
}

task_count=$(find "$epic_dir" -maxdepth 1 -type f -name '[0-9]*.md' | awk 'END { print NR }')
if [ "$task_count" -eq 0 ]; then
  echo "❌ No tasks to sync. Run: /pm:epic-decompose $ARGUMENTS" >&2
  exit 1
fi

epic_sync_parent="${MONDAY_EPIC_SYNC_TMP_PARENT:-${TMPDIR:-/tmp}}"
if ! mkdir -p "$epic_sync_parent" ||
  ! epic_sync_tmp=$(mktemp -d "$epic_sync_parent/monday-epic-sync.XXXXXX"); then
  echo "❌ Could not create a fresh epic-sync scratch directory" >&2
  exit 1
fi
export MONDAY_EPIC_SYNC_TMP="$epic_sync_tmp"
```

Keep `MONDAY_EPIC_SYNC_TMP` in the controller environment for every step and
parallel batch. Each run owns a fresh scratch root; an optional
`MONDAY_EPIC_SYNC_TMP_PARENT` chooses only its parent directory.

## Native Relationship Preflight

Run this before any `gh issue create`:

```bash
if ! gh issue create --help | grep -q -- '--parent' ||
  ! gh issue edit --help | grep -q -- '--add-blocked-by'; then
  echo "❌ gh lacks native --parent or --add-blocked-by support; upgrade GitHub CLI" >&2
  exit 1
fi
```

## Instructions

### 1. Create or Resume Epic Issue

Strip frontmatter and prepare GitHub issue body:
```bash
epic_sync_tmp="${MONDAY_EPIC_SYNC_TMP:?Run the Quick Check first}"
: > "$epic_sync_tmp/task-mapping.txt"

# Extract content without frontmatter
sed '1,/^---$/d; 1,/^---$/d' "$epic_dir/epic.md" \
  > "$epic_sync_tmp/epic-body-raw.md"

# Remove "## Tasks Created" section and replace with Stats
awk '
  /^## Tasks Created/ { 
    in_tasks=1
    next
  }
  /^## / && in_tasks { 
    in_tasks=0
    # When we hit the next section after Tasks Created, add Stats
    if (total_tasks) {
      print "## Stats\n"
      print "Total tasks: " total_tasks
      print "Parallel tasks: " parallel_tasks " (can be worked on simultaneously)"
      print "Sequential tasks: " sequential_tasks " (have dependencies)"
      if (total_effort) print "Estimated total effort: " total_effort " hours"
      print ""
    }
  }
  /^Total tasks:/ && in_tasks { total_tasks = $3; next }
  /^Parallel tasks:/ && in_tasks { parallel_tasks = $3; next }
  /^Sequential tasks:/ && in_tasks { sequential_tasks = $3; next }
  /^Estimated total effort:/ && in_tasks { 
    gsub(/^Estimated total effort: /, "")
    total_effort = $0
    next 
  }
  !in_tasks { print }
  END {
    # If we were still in tasks section at EOF, add stats
    if (in_tasks && total_tasks) {
      print "## Stats\n"
      print "Total tasks: " total_tasks
      print "Parallel tasks: " parallel_tasks " (can be worked on simultaneously)"
      print "Sequential tasks: " sequential_tasks " (have dependencies)"
      if (total_effort) print "Estimated total effort: " total_effort
    }
  }
' "$epic_sync_tmp/epic-body-raw.md" > "$epic_sync_tmp/epic-body.md"

epic_marker="<!-- monday-source: epic:$ARGUMENTS -->"
printf '\n%s\n' "$epic_marker" >> "$epic_sync_tmp/epic-body.md"
issue_category="enhancement"

if ! epic_urls=$(gh api --paginate 'repos/{owner}/{repo}/issues?state=all&per_page=100' \
  --jq ".[] | select((has(\"pull_request\") | not) and .body != null and (.body | contains(\"$epic_marker\"))) | .html_url"); then
  echo "❌ Could not look up an existing tracking issue" >&2
  exit 1
fi
epic_match_count=$(printf '%s\n' "$epic_urls" | awk 'NF { count++ } END { print count + 0 }')
if [ "$epic_match_count" -gt 1 ]; then
  echo "❌ Multiple tracking issues use $epic_marker" >&2
  exit 1
elif [ "$epic_match_count" -eq 1 ]; then
  epic_url="$epic_urls"
elif ! epic_url=$(gh issue create \
  --title "Epic: $ARGUMENTS" \
  --body-file "$epic_sync_tmp/epic-body.md" \
  --label "$issue_category,needs-triage,tracking"); then
  echo "❌ Could not create the tracking issue" >&2
  exit 1
fi

epic_number="${epic_url##*/}"
case "$epic_number" in
  ''|*[!0-9]*) echo "❌ Could not parse issue number from $epic_url" >&2; exit 1 ;;
esac

if ! published_epic_body=$(gh issue view "$epic_number" --json body --jq .body) ||
  ! published_epic_labels=$(gh issue view "$epic_number" --json labels --jq '.labels[].name'); then
  echo "❌ Could not read back the published tracking issue" >&2
  exit 1
fi
epic_category_count=$(printf '%s\n' "$published_epic_labels" | awk '$0 == "bug" || $0 == "enhancement" { count++ } END { print count + 0 }')
epic_state_count=$(printf '%s\n' "$published_epic_labels" | awk '/^(needs-triage|needs-info|ready-for-agent|ready-for-human|wontfix)$/ { count++ } END { print count + 0 }')
if [ "$published_epic_body" != "$(cat "$epic_sync_tmp/epic-body.md")" ] ||
  [ "$epic_category_count" -ne 1 ] || [ "$epic_state_count" -ne 1 ] ||
  ! printf '%s\n' "$published_epic_labels" | grep -Fxq enhancement ||
  ! printf '%s\n' "$published_epic_labels" | grep -Fxq tracking; then
  echo "❌ Published tracking issue body or labels do not match" >&2
  exit 1
fi
```

Store the returned issue number for epic frontmatter update.

### 2. Create or Resume Native Task Sub-Issues

```bash
epic_sync_tmp="${MONDAY_EPIC_SYNC_TMP:?Run the Quick Check first}"
if [ "$task_count" -lt 5 ]; then
  for task_file in "$epic_dir"/[0-9]*.md; do
    [ -f "$task_file" ] || continue

    source_id=$(task_source_id "$task_file")
    task_name=$(grep '^name:' "$task_file" | sed 's/^name: *//')
    task_marker="<!-- monday-source: epic:$ARGUMENTS/task:$source_id -->"
    task_body="$epic_sync_tmp/task-$source_id-body.md"
    sed '1,/^---$/d; 1,/^---$/d' "$task_file" > "$task_body"
    printf '\n%s\n' "$task_marker" >> "$task_body"

    if ! task_urls=$(gh api --paginate 'repos/{owner}/{repo}/issues?state=all&per_page=100' \
      --jq ".[] | select((has(\"pull_request\") | not) and .body != null and (.body | contains(\"$task_marker\"))) | .html_url"); then
      echo "❌ Could not look up a published issue for $task_file" >&2
      exit 1
    fi
    task_match_count=$(printf '%s\n' "$task_urls" | awk 'NF { count++ } END { print count + 0 }')
    if [ "$task_match_count" -gt 1 ]; then
      echo "❌ Multiple issues use $task_marker" >&2
      exit 1
    elif [ "$task_match_count" -eq 1 ]; then
      task_url="$task_urls"
    elif ! task_url=$(gh issue create \
      --parent "$epic_number" \
      --title "$task_name" \
      --body-file "$task_body" \
      --label "$issue_category,needs-triage"); then
      echo "❌ Could not create a native sub-issue for $task_file" >&2
      exit 1
    fi

    task_number="${task_url##*/}"
    case "$task_number" in
      ''|*[!0-9]*) echo "❌ Could not parse issue number from $task_url" >&2; exit 1 ;;
    esac
    printf '%s:%s\n' "$task_file" "$task_number" >> "$epic_sync_tmp/task-mapping.txt"
  done
fi
```

### For Larger Batches: Parallel Creation

```bash
if [ "$task_count" -ge 5 ]; then
  echo "Creating $task_count sub-issues in parallel..."
fi
```

Use Task tool for parallel creation:
```yaml
Task:
  description: "Create GitHub sub-issues batch {X}"
  subagent_type: "general-purpose"
  prompt: |
    Create GitHub sub-issues for tasks in epic $ARGUMENTS
    Parent epic issue: #$epic_number
    Scratch root: $epic_sync_tmp/batch-{X}
    
    Tasks to process:
    - {list of 3-4 task files}
    
    For each task file:
    1. Extract task name and stable source ID (`monday_source`, otherwise filename)
    2. Strip frontmatter into the batch body file, then append the stable marker:
       <!-- monday-source: epic:$ARGUMENTS/task:{source_id} -->
    3. Search every paginated issue page for that exact marker. Fail if more than
       one matches; resume the one match; otherwise create with:
       gh issue create --parent $epic_number --title "$task_name" \
         --body-file {batch_body_file} \
         --label "$issue_category,needs-triage"
    4. Parse the returned or resumed URL and record exactly one line:
       task_file:issue_number
       in "$epic_sync_tmp/batch-{X}/mapping.txt".
    
    IMPORTANT: Stop on lookup or creation failure. Do not rename source files.
    
    Return mapping of files to issue numbers.
```

Consolidate results from parallel agents:
```bash
epic_sync_tmp="${MONDAY_EPIC_SYNC_TMP:?Run the Quick Check first}"
for batch_mapping in "$epic_sync_tmp"/batch-*/mapping.txt; do
  [ -f "$batch_mapping" ] || continue
  cat "$batch_mapping" >> "$epic_sync_tmp/task-mapping.txt"
done
```

### 2b. Validate Complete Mapping

Do this before any relationship readback or source rename:

```bash
epic_sync_tmp="${MONDAY_EPIC_SYNC_TMP:?Run the Quick Check first}"
mapping_file="$epic_sync_tmp/task-mapping.txt"
expected_mapping_count="$task_count"
actual_mapping_count=$(awk 'NF { count++ } END { print count + 0 }' "$mapping_file")
if [ "$actual_mapping_count" -ne "$expected_mapping_count" ]; then
  echo "❌ Expected $expected_mapping_count mappings, got $actual_mapping_count" >&2
  exit 1
fi

while IFS=: read -r mapped_file mapped_number extra; do
  case "$mapped_file:$mapped_number:$extra" in
    "$epic_dir"/[0-9]*.md:[0-9]*:) ;;
    *) echo "❌ Invalid task mapping: $mapped_file:$mapped_number" >&2; exit 1 ;;
  esac
  if [ ! -f "$mapped_file" ]; then
    echo "❌ Invalid task mapping: $mapped_file:$mapped_number" >&2
    exit 1
  fi
  case "$mapped_number" in
    ''|*[!0-9]*) echo "❌ Invalid issue number: $mapped_number" >&2; exit 1 ;;
  esac
done < "$mapping_file"

unique_issue_count=$(awk -F: 'NF && !seen[$2]++ { count++ } END { print count + 0 }' "$mapping_file")
if [ "$unique_issue_count" -ne "$expected_mapping_count" ]; then
  echo "❌ Task mappings reuse a GitHub issue number" >&2
  exit 1
fi
for task_file in "$epic_dir"/[0-9]*.md; do
  source_mapping_count=$(awk -F: -v source="$task_file" '$1 == source { count++ } END { print count + 0 }' "$mapping_file")
  if [ "$source_mapping_count" -ne 1 ]; then
    echo "❌ $task_file has $source_mapping_count mappings" >&2
    exit 1
  fi
done
```

### 2c. Verify Native Publications

Read back exact bodies, labels, and native parent relationships:

```bash
epic_sync_tmp="${MONDAY_EPIC_SYNC_TMP:?Run the Quick Check first}"
mapping_file="$epic_sync_tmp/task-mapping.txt"
if ! published_subissues=$(gh issue view "$epic_number" --json subIssues \
  --jq '.subIssues.nodes[].number'); then
  echo "❌ Could not read back native sub-issues" >&2
  exit 1
fi
expected_subissues=$(awk -F: 'NF { print $2 }' "$mapping_file" | LC_ALL=C sort -n)
actual_subissues=$(printf '%s\n' "$published_subissues" | awk 'NF' | LC_ALL=C sort -n)
if [ "$actual_subissues" != "$expected_subissues" ]; then
  echo "❌ Native sub-issue membership does not match this publication" >&2
  exit 1
fi

while IFS=: read -r task_file task_number; do
  source_id=$(task_source_id "$task_file")
  expected_task_body="$epic_sync_tmp/expected-task-$source_id.md"
  sed '1,/^---$/d; 1,/^---$/d' "$task_file" > "$expected_task_body"
  printf '\n<!-- monday-source: epic:%s/task:%s -->\n' "$ARGUMENTS" "$source_id" \
    >> "$expected_task_body"

  if ! published_task_body=$(gh issue view "$task_number" --json body --jq .body) ||
    ! published_task_labels=$(gh issue view "$task_number" --json labels --jq '.labels[].name') ||
    ! published_parent=$(gh issue view "$task_number" --json parent --jq '.parent.number'); then
    echo "❌ Could not read back task #$task_number" >&2
    exit 1
  fi
  task_category_count=$(printf '%s\n' "$published_task_labels" | awk '$0 == "bug" || $0 == "enhancement" { count++ } END { print count + 0 }')
  task_state_count=$(printf '%s\n' "$published_task_labels" | awk '/^(needs-triage|needs-info|ready-for-agent|ready-for-human|wontfix)$/ { count++ } END { print count + 0 }')
  if [ "$published_task_body" != "$(cat "$expected_task_body")" ] ||
    [ "$task_category_count" -ne 1 ] || [ "$task_state_count" -ne 1 ] ||
    ! printf '%s\n' "$published_task_labels" | grep -Fxq enhancement ||
    [ "$published_parent" != "$epic_number" ]; then
    echo "❌ Published task #$task_number does not match body, labels, or parent" >&2
    exit 1
  fi
done < "$mapping_file"
```

### 3. Publish and Verify Native Dependencies

Resolve source `depends_on` values through the validated mapping. Publish and
read back the exact blocked-by set before touching local filenames:

```bash
epic_sync_tmp="${MONDAY_EPIC_SYNC_TMP:?Run the Quick Check first}"
mapping_file="$epic_sync_tmp/task-mapping.txt"
while IFS=: read -r task_file task_number; do
  expected_blockers="$epic_sync_tmp/expected-blockers-$task_number.txt"
  : > "$expected_blockers"
  dependencies=$(sed -n 's/^depends_on: *\[\(.*\)\].*/\1/p' "$task_file" | tr -d ' ')
  old_ifs=$IFS
  IFS=,
  for source_dependency in $dependencies; do
    dependency_file="$epic_dir/$source_dependency.md"
    if [ ! -f "$dependency_file" ]; then
      dependency_file=$(grep -lFx "monday_source: $source_dependency" "$epic_dir"/[0-9]*.md || true)
      case "$dependency_file" in *$'\n'*) echo "❌ Duplicate source task $source_dependency" >&2; exit 1 ;; esac
    fi
    dependency_number=$(awk -F: -v source="$dependency_file" '$1 == source { print $2 }' "$mapping_file")
    case "$dependency_number" in
      ''|*[!0-9]*)
        echo "❌ No published issue for dependency $source_dependency" >&2
        exit 1
        ;;
    esac
    printf '%s\n' "$dependency_number" >> "$expected_blockers"
  done
  IFS=$old_ifs
  LC_ALL=C sort -nu -o "$expected_blockers" "$expected_blockers"

  if ! published_blockers=$(gh issue view "$task_number" --json blockedBy \
    --jq '.blockedBy.nodes[].number'); then
    echo "❌ Could not read blockers for #$task_number" >&2
    exit 1
  fi
  while IFS= read -r dependency_number; do
    [ -n "$dependency_number" ] || continue
    if ! printf '%s\n' "$published_blockers" | grep -Fxq "$dependency_number"; then
      if ! gh issue edit "$task_number" --add-blocked-by "$dependency_number"; then
        echo "❌ Could not publish #$task_number blocked by #$dependency_number" >&2
        exit 1
      fi
    fi
  done < "$expected_blockers"

  if ! published_blockers=$(gh issue view "$task_number" --json blockedBy \
    --jq '.blockedBy.nodes[].number'); then
    echo "❌ Could not verify blockers for #$task_number" >&2
    exit 1
  fi
  actual_blockers=$(printf '%s\n' "$published_blockers" | awk 'NF' | LC_ALL=C sort -nu)
  if [ "$actual_blockers" != "$(cat "$expected_blockers")" ]; then
    echo "❌ Native blockers for #$task_number do not match depends_on" >&2
    exit 1
  fi
done < "$mapping_file"
```

### 4. Rename Task Files and Update References

First, build a mapping of old numbers to new issue IDs:
```bash
epic_sync_tmp="${MONDAY_EPIC_SYNC_TMP:?Run the Quick Check first}"
# Create mapping from old task numbers (001, 002, etc.) to new issue IDs
> "$epic_sync_tmp/id-mapping.txt"
while IFS=: read -r task_file task_number; do
  old_num=$(task_source_id "$task_file")
  echo "$old_num:$task_number" >> "$epic_sync_tmp/id-mapping.txt"
done < "$epic_sync_tmp/task-mapping.txt"
```

Then rename files and update all references:
```bash
epic_sync_tmp="${MONDAY_EPIC_SYNC_TMP:?Run the Quick Check first}"
# Process each task file
while IFS=: read -r task_file task_number; do
  new_name="$(dirname "$task_file")/${task_number}.md"
  
  # Read the file content
  content=$(cat "$task_file")
  source_id=$(task_source_id "$task_file")
  grep -q '^monday_source:' "$task_file" ||
    content=$(printf '%s\n' "$content" | awk -v id="$source_id" 'NR == 2 { print "monday_source: " id } { print }')
  
  # Update depends_on and conflicts_with references
  while IFS=: read -r old_num new_num; do
    # Update arrays like [001, 002] to use new issue numbers
    content=$(echo "$content" | sed -E "s/(\[|, ?)$old_num(,|\])/\1$new_num\2/g")
  done < "$epic_sync_tmp/id-mapping.txt"
  
  # Write updated content to new file
  echo "$content" > "$new_name"
  
  # Remove old file if different from new
  [ "$task_file" != "$new_name" ] && rm "$task_file"
  
  # Update github field in frontmatter
  # Add the GitHub URL to the frontmatter
  repo=$(gh repo view --json nameWithOwner -q .nameWithOwner)
  github_url="https://github.com/$repo/issues/$task_number"
  
  # Update frontmatter with GitHub URL and current timestamp
  current_date=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
  
  # Use sed to update the github and updated fields
  sed -i.bak "/^github:/c\github: $github_url" "$new_name"
  sed -i.bak "/^updated:/c\updated: $current_date" "$new_name"
  rm "${new_name}.bak"
done < "$epic_sync_tmp/task-mapping.txt"
```

### 5. Update Epic File

Update the epic file with GitHub URL, timestamp, and real task IDs:

#### 5a. Update Frontmatter
```bash
# Get repo info
repo=$(gh repo view --json nameWithOwner -q .nameWithOwner)
epic_url="https://github.com/$repo/issues/$epic_number"
current_date=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

# Update epic frontmatter
sed -i.bak "/^github:/c\github: $epic_url" .claude/epics/$ARGUMENTS/epic.md
sed -i.bak "/^updated:/c\updated: $current_date" .claude/epics/$ARGUMENTS/epic.md
rm .claude/epics/$ARGUMENTS/epic.md.bak
```

#### 5b. Update Tasks Created Section
```bash
epic_sync_tmp="${MONDAY_EPIC_SYNC_TMP:?Run the Quick Check first}"
# Create a temporary file with the updated Tasks Created section
cat > "$epic_sync_tmp/tasks-section.md" << 'EOF'
## Tasks Created
EOF

# Add each task with its real issue number
for task_file in .claude/epics/$ARGUMENTS/[0-9]*.md; do
  [ -f "$task_file" ] || continue
  
  # Get issue number (filename without .md)
  issue_num=$(basename "$task_file" .md)
  
  # Get task name from frontmatter
  task_name=$(grep '^name:' "$task_file" | sed 's/^name: *//')
  
  # Get parallel status
  parallel=$(grep '^parallel:' "$task_file" | sed 's/^parallel: *//')
  
  # Add to tasks section
  echo "- [ ] #${issue_num} - ${task_name} (parallel: ${parallel})" >> "$epic_sync_tmp/tasks-section.md"
done

# Add summary statistics
total_count=$(ls .claude/epics/$ARGUMENTS/[0-9]*.md 2>/dev/null | wc -l)
parallel_count=$(grep -l '^parallel: true' .claude/epics/$ARGUMENTS/[0-9]*.md 2>/dev/null | wc -l)
sequential_count=$((total_count - parallel_count))

cat >> "$epic_sync_tmp/tasks-section.md" << EOF

Total tasks: ${total_count}
Parallel tasks: ${parallel_count}
Sequential tasks: ${sequential_count}
EOF

# Replace the Tasks Created section in epic.md
# First, create a backup
cp .claude/epics/$ARGUMENTS/epic.md .claude/epics/$ARGUMENTS/epic.md.backup

# Use awk to replace the section
awk -v tasks_section="$epic_sync_tmp/tasks-section.md" '
  /^## Tasks Created/ { 
    skip=1
    while ((getline line < tasks_section) > 0) print line
    close(tasks_section)
  }
  /^## / && !/^## Tasks Created/ { skip=0 }
  !skip && !/^## Tasks Created/ { print }
' .claude/epics/$ARGUMENTS/epic.md.backup > .claude/epics/$ARGUMENTS/epic.md

# Clean up
rm .claude/epics/$ARGUMENTS/epic.md.backup
rm "$epic_sync_tmp/tasks-section.md"
```

### 6. Create Mapping File

Create `.claude/epics/$ARGUMENTS/github-mapping.md`:
```bash
# Create mapping file
cat > .claude/epics/$ARGUMENTS/github-mapping.md << EOF
# GitHub Issue Mapping

Epic: #${epic_number} - https://github.com/${repo}/issues/${epic_number}

Tasks:
EOF

# Add each task mapping
for task_file in .claude/epics/$ARGUMENTS/[0-9]*.md; do
  [ -f "$task_file" ] || continue
  
  issue_num=$(basename "$task_file" .md)
  task_name=$(grep '^name:' "$task_file" | sed 's/^name: *//')
  
  echo "- #${issue_num}: ${task_name} - https://github.com/${repo}/issues/${issue_num}" >> .claude/epics/$ARGUMENTS/github-mapping.md
done

# Add sync timestamp
echo "" >> .claude/epics/$ARGUMENTS/github-mapping.md
echo "Synced: $(date -u +"%Y-%m-%dT%H:%M:%SZ")" >> .claude/epics/$ARGUMENTS/github-mapping.md
```

### 7. Hand off ready issues

Do not create an epic-wide writable worktree here. Each ready issue creates
its dedicated worktree through `/rules/worktree-operations.md`.

### 8. Output

```
✅ Synced to GitHub
  - Epic: #{epic_number} - {epic_title}
  - Tasks: {count} sub-issues created
  - Labels applied: enhancement + needs-triage; tracking on the epic
  - Files renamed: 001.md → {issue_id}.md
  - References updated: depends_on/conflicts_with now use issue IDs
  - Worktrees: one dedicated path per ready issue

Next steps:
  - Start parallel execution: /pm:epic-start $ARGUMENTS
  - Or work on single issue: /pm:issue-start {issue_number}
  - View epic: https://github.com/{owner}/{repo}/issues/{epic_number}
```

## Error Handling

Follow `/rules/github-operations.md` for GitHub CLI errors.

If any issue creation fails:
- Report what succeeded
- Note what failed
- Don't attempt rollback (partial sync is fine)

## Important Notes

- Trust GitHub CLI authentication
- Resume issues by their stable `monday-source` marker
- Update frontmatter only after successful creation
- Keep operations simple and atomic
