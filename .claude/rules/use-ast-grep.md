# AST-Grep Integration Protocol for Cursor Agent

## When to Use AST-Grep

Use `ast-grep` (if installed) instead of plain regex or text search when:

- **Structural code patterns** are involved (e.g., finding all function calls, class definitions, or method implementations)
- **Language-aware refactoring** is required (e.g., renaming variables, updating function signatures, or changing imports)
- **Complex code analysis** is needed (e.g., finding all usages of a pattern across different syntactic contexts)
- **Cross-language searches** are necessary (e.g., working with both Ruby and TypeScript in a monorepo)
- **Semantic code understanding** is important (e.g., finding patterns based on code structure, not just text)

## Integration Workflow

### Before using ast-grep:
1. **Check if ast-grep is installed:**
   If not, skip and fall back to regex/semantic search.
   ```sh
   command -v ast-grep >/dev/null 2>&1 || echo "ast-grep not installed, skipping AST search"
   ```
2. **Identify** if the task involves structural code patterns or language-aware refactoring.
3. **Determine** the appropriate language(s) to search.
4. **Construct** the pattern using ast-grep syntax.
5. **Run** ast-grep to gather precise structural information.
6. **Use** results to inform code edits, refactoring, or further analysis.

### Example Workflow

When asked to "find all Ruby service objects that call `perform`":

1. **Check for ast-grep:**
   ```sh
   command -v ast-grep >/dev/null 2>&1 && ast-grep --pattern 'perform($$$)' --lang ruby app/services/
   ```
2. **Analyze** results structurally.
3. **Use** codebase semantic search for additional context if needed.
4. **Make** informed edits based on structural understanding.

### Combine ast-grep with Internal Tools

- **codebase_search** for semantic context and documentation
- **read_file** for examining specific files found by ast-grep
- **edit_file** for making precise, context-aware code changes

### Decision Matrix: When to Use Each Tool

| Task Type                | Tool Choice          | Reason                        |
|--------------------------|----------------------|-------------------------------|
| Find text patterns       | grep_search          | Simple text matching          |
| Find code structures     | ast-grep             | Syntax-aware search           |
| Understand semantics     | codebase_search      | AI-powered context            |
| Make edits               | edit_file            | Precise file editing          |
| Structural refactoring   | ast-grep + edit_file | Structure + precision         |

**Always prefer ast-grep for code structure analysis over regex-based approaches, but only if it is installed and available.**
