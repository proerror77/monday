# Branch Operations

Use a branch when a change will be published or must be isolated from concurrent
writes. A branch has one write owner and contains one independently reviewable
behavior.

Re-read branch, `HEAD`, status, and PR head before publishing or merging. Do not
use another contract's branch as a synchronization mechanism; depend on its
merged result or an explicit stack.

Do not delete a branch or worktree without explicit repository-owner
authorization for the exact target.
