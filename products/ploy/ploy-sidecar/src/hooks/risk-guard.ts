/**
 * Deployment Guard Hook — keeps the sidecar on control-plane-safe paths.
 *
 * Runs BEFORE the apply_deployment tool executes. Denies non-paper runtime
 * modes unless the operator explicitly opts in.
 */

export interface RiskGuardInput {
  hook_event_name: string;
  tool_name: string;
  tool_input: Record<string, unknown>;
}

export interface RiskGuardOutput {
  hookSpecificOutput?: {
    hookEventName: string;
    permissionDecision: "allow" | "deny";
    permissionDecisionReason?: string;
    updatedInput?: Record<string, unknown>;
  };
}

export async function riskGuardHook(
  input: RiskGuardInput
): Promise<RiskGuardOutput> {
  if (input.hook_event_name !== "PreToolUse") return {};
  if (!input.tool_name.includes("apply_deployment")) return {};

  const toolInput = input.tool_input as {
    runtime_mode?: string;
  };
  const runtimeMode = toolInput.runtime_mode?.trim() || "paper";
  const allowNonPaper = process.env.SIDECAR_ALLOW_NON_PAPER_DEPLOYMENTS === "true";

  if (!allowNonPaper && runtimeMode !== "paper") {
    return {
      hookSpecificOutput: {
        hookEventName: "PreToolUse",
        permissionDecision: "deny",
        permissionDecisionReason:
          `runtime_mode=${runtimeMode} is blocked; set SIDECAR_ALLOW_NON_PAPER_DEPLOYMENTS=true to override`,
      },
    };
  }

  return {};
}
