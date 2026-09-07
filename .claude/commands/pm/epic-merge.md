---
allowed-tools: Bash, Read, Write, LS
---

# Epic Merge

Apply the shared [delivery authority](../../../AGENTS.md#delivery-authority).
For an authorized epic merge, process its PRs in their declared dependency order.
After each merge, read back its identity and synchronize its originating checkout
when safe, following [ownership](../../../AGENTS.md#scope-and-ownership).
