import assert from "node:assert/strict";
import { createHash, createHmac, randomUUID, timingSafeEqual } from "node:crypto";
import { execFile, spawn } from "node:child_process";
import { appendFile, chmod, mkdir, mkdtemp, readFile, rename, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, dirname, join, resolve } from "node:path";
import { promisify } from "node:util";
import { pathToFileURL } from "node:url";

import { harnessSelfModificationsPath } from "./session-store.js";

const execFileAsync = promisify(execFile);

export type SelfModificationProposal = {
  kind: "self_modification_proposal";
  proposal_id: string;
  title: string;
  patch: string;
  verification_command: string;
  verification_profile: string;
  patch_hash: string;
  status: "proposed" | "applied";
  created_at: string;
  applied_at?: string;
  branch_name?: string;
  commit_sha?: string;
  pull_request_url?: string;
};

type WorkflowInputs = Record<string, string | number | boolean>;

export type SelfModificationDeploymentDispatch = {
  kind: "self_modification_deployment_dispatch";
  deployment_id: string;
  workflow: string;
  workflow_ref: string;
  inputs: WorkflowInputs;
  rollback_workflow: string;
  rollback_workflow_ref: string;
  rollback_inputs: WorkflowInputs;
  status: "dispatched";
  run_url?: string;
  created_at: string;
};

export type SelfModificationRollbackDispatch = {
  kind: "self_modification_rollback_dispatch";
  deployment_id: string;
  workflow: string;
  workflow_ref: string;
  inputs: WorkflowInputs;
  status: "dispatched";
  run_url?: string;
  created_at: string;
};

export async function proposeSelfModification(params: {
  title: string;
  patch: string;
  verification_command?: string;
}): Promise<SelfModificationProposal> {
  const proposal: SelfModificationProposal = {
    kind: "self_modification_proposal",
    proposal_id: `selfmod-${randomUUID()}`,
    title: params.title,
    patch: params.patch,
    verification_command: "profile:sidecar_frontend_build",
    verification_profile: "sidecar_frontend_build",
    patch_hash: createHash("sha256").update(params.patch).digest("hex"),
    status: "proposed",
    created_at: new Date().toISOString(),
  };
  await appendFile(await harnessSelfModificationsPath(), `${JSON.stringify(proposal)}\n`, "utf8");
  return proposal;
}

export async function dispatchApprovedSelfModificationDeployment(params: {
  approval_token: string;
  workflow: string;
  workflow_ref?: string;
  inputs?: WorkflowInputs;
  rollback_workflow: string;
  rollback_workflow_ref?: string;
  rollback_inputs?: WorkflowInputs;
}): Promise<SelfModificationDeploymentDispatch> {
  verifySelfModificationApproval(params.approval_token);
  if (process.env.PLOY_HARNESS_SELF_MOD_ALLOW_DEPLOY !== "true") {
    throw new Error("self-modification deployment dispatch is not enabled");
  }
  const workflowRef = params.workflow_ref || "main";
  const rollbackWorkflowRef = params.rollback_workflow_ref || "main";
  const inputs = params.inputs || {};
  const rollbackInputs = params.rollback_inputs || {};
  validateWorkflowAllowed(params.workflow);
  validateWorkflowAllowed(params.rollback_workflow);
  validateWorkflowRef(workflowRef, inputs);
  validateWorkflowRef(rollbackWorkflowRef, rollbackInputs);
  validateWorkflowInputs(inputs);
  validateWorkflowInputs(rollbackInputs);

  const repoRoot = await findRepoRoot();
  const runUrl = await dispatchWorkflow(repoRoot, params.workflow, workflowRef, inputs);
  const record: SelfModificationDeploymentDispatch = {
    kind: "self_modification_deployment_dispatch",
    deployment_id: `selfdeploy-${randomUUID()}`,
    workflow: params.workflow,
    workflow_ref: workflowRef,
    inputs,
    rollback_workflow: params.rollback_workflow,
    rollback_workflow_ref: rollbackWorkflowRef,
    rollback_inputs: rollbackInputs,
    status: "dispatched",
    run_url: runUrl,
    created_at: new Date().toISOString(),
  };
  await appendFile(await harnessSelfModificationsPath(), `${JSON.stringify(record)}\n`, "utf8");
  return record;
}

export async function dispatchApprovedSelfModificationRollback(params: {
  approval_token: string;
  workflow: string;
  workflow_ref?: string;
  inputs?: WorkflowInputs;
}): Promise<SelfModificationRollbackDispatch> {
  verifySelfModificationApproval(params.approval_token);
  if (process.env.PLOY_HARNESS_SELF_MOD_ALLOW_DEPLOY !== "true") {
    throw new Error("self-modification rollback dispatch is not enabled");
  }
  const workflowRef = params.workflow_ref || "main";
  const inputs = params.inputs || {};
  validateWorkflowAllowed(params.workflow);
  validateWorkflowRef(workflowRef, inputs);
  validateWorkflowInputs(inputs);

  const repoRoot = await findRepoRoot();
  const runUrl = await dispatchWorkflow(repoRoot, params.workflow, workflowRef, inputs);
  const record: SelfModificationRollbackDispatch = {
    kind: "self_modification_rollback_dispatch",
    deployment_id: `selfrollback-${randomUUID()}`,
    workflow: params.workflow,
    workflow_ref: workflowRef,
    inputs,
    status: "dispatched",
    run_url: runUrl,
    created_at: new Date().toISOString(),
  };
  await appendFile(await harnessSelfModificationsPath(), `${JSON.stringify(record)}\n`, "utf8");
  return record;
}

export async function applyApprovedSelfModification(params: {
  proposal_id: string;
  approval_token: string;
  publish_pull_request?: boolean;
  branch_name?: string;
  commit_message?: string;
  pull_request_title?: string;
  pull_request_body?: string;
  base_branch?: string;
}): Promise<SelfModificationProposal> {
  const proposal = await readProposal(params.proposal_id);
  verifySelfModificationProposalApproval(proposal, params.approval_token);
  const repoRoot = await findRepoRoot();
  await ensureCleanWorktree(repoRoot);
  const publishPullRequest = params.publish_pull_request === true;
  const branchName = publishPullRequest
    ? params.branch_name || `selfmod/${proposal.proposal_id.slice(-12)}`
    : undefined;
  const originalBranch = publishPullRequest ? await currentBranch(repoRoot) : undefined;
  let switchedBranch = false;
  let patchApplied = false;
  let committed = false;
  let pushedBranch = false;

  try {
    if (publishPullRequest) {
      if (process.env.PLOY_HARNESS_SELF_MOD_ALLOW_PR !== "true") {
        throw new Error("self-modification PR publishing is not enabled");
      }
      if (!branchName) throw new Error("self-modification PR branch name is missing");
      await run(repoRoot, "git", ["check-ref-format", "--branch", branchName]);
      await run(repoRoot, "git", ["switch", "-c", branchName]);
      switchedBranch = true;
    }
    await run(repoRoot, "git", ["apply", "--check", "-"], proposal.patch);
    await run(repoRoot, "git", ["apply", "-"], proposal.patch);
    patchApplied = true;
    await runVerificationProfile(repoRoot, proposal.verification_profile);
    if (publishPullRequest) {
      await run(repoRoot, "git", ["add", "--", ...patchPaths(proposal.patch)]);
      await run(repoRoot, "git", ["diff", "--cached", "--quiet"]).then(
        () => {
          throw new Error("self-modification patch produced no staged changes");
        },
        () => undefined
      );
      await run(repoRoot, "git", [
        "commit",
        "-m",
        params.commit_message || proposal.title,
      ]);
      committed = true;
      const commitSha = (await runOutput(repoRoot, "git", ["rev-parse", "HEAD"])).trim();
      await run(repoRoot, "git", ["push", "-u", "origin", branchName!]);
      pushedBranch = true;
      const prUrl = (
        await runOutput(repoRoot, "gh", [
          "pr",
          "create",
          "--base",
          params.base_branch || "main",
          "--head",
          branchName!,
          "--title",
          params.pull_request_title || proposal.title,
          "--body",
          params.pull_request_body ||
            [
              "Approval-gated harness self-modification.",
              "",
              `Proposal: ${proposal.proposal_id}`,
              `Verification: ${proposal.verification_command}`,
              "",
              "Deployment: do not deploy from this branch; merge to main and use the repo's existing deployment workflows.",
            ].join("\n"),
        ])
      ).trim();
      return recordApplied(proposal, {
        branch_name: branchName,
        commit_sha: commitSha,
        pull_request_url: prUrl,
      });
    }
  } catch (error) {
    if (patchApplied && !committed) {
      await run(repoRoot, "git", ["apply", "-R", "-"], proposal.patch).catch(() => undefined);
    }
    if (switchedBranch && originalBranch) {
      await run(repoRoot, "git", ["switch", originalBranch]).catch(() => undefined);
      await run(repoRoot, "git", ["branch", "-D", branchName!]).catch(() => undefined);
    }
    if (pushedBranch) {
      await run(repoRoot, "git", ["push", "origin", `:${branchName}`]).catch(() => undefined);
    }
    throw error;
  }

  return recordApplied(proposal, {});
}

function verifySelfModificationApproval(approvalToken: string): void {
  const expectedToken = process.env.PLOY_HARNESS_SELF_MOD_APPROVAL_TOKEN;
  if (!expectedToken) {
    throw new Error("self-modification approval is not configured");
  }
  if (approvalToken !== expectedToken) {
    throw new Error("invalid self-modification approval token");
  }
}

export function selfModificationApprovalProof(proposal: Pick<SelfModificationProposal, "proposal_id" | "patch_hash" | "verification_profile">, secret: string): string {
  return createHmac("sha256", secret)
    .update(`${proposal.proposal_id}\n${proposal.patch_hash}\n${proposal.verification_profile}`)
    .digest("hex");
}

export function verifySelfModificationProposalApproval(proposal: SelfModificationProposal, proof: string): void {
  const secret = process.env.PLOY_HARNESS_SELF_MOD_APPROVAL_TOKEN;
  if (!secret) throw new Error("self-modification approval is not configured");
  const actualPatchHash = createHash("sha256").update(proposal.patch).digest("hex");
  if (actualPatchHash !== proposal.patch_hash) {
    throw new Error("self-modification proposal patch hash mismatch");
  }
  const expected = Buffer.from(selfModificationApprovalProof(proposal, secret), "hex");
  const actual = Buffer.from(proof, "hex");
  if (actual.length !== expected.length || !timingSafeEqual(actual, expected)) {
    throw new Error("invalid self-modification approval proof");
  }
}

async function runVerificationProfile(cwd: string, profile: string): Promise<void> {
  const profiles: Record<string, Array<[string, string[]]>> = {
    sidecar_frontend_build: [
      ["npm", ["run", "build", "--prefix", "ploy-sidecar"]],
      ["npm", ["run", "build", "--prefix", "ploy-frontend"]],
    ],
    sidecar_test: [["npm", ["test", "--prefix", "ploy-sidecar"]]],
  };
  const selected = profiles[profile];
  if (!selected) throw new Error(`unsupported verification profile: ${profile}`);
  for (const [command, args] of selected) {
    await execFileAsync(command, args, { cwd, maxBuffer: 1024 * 1024 * 20 });
  }
}

function patchPaths(patch: string): string[] {
  const paths = [
    ...[...patch.matchAll(/^--- (?:a\/)?(.+)$/gm)].map((match) => match[1]),
    ...[...patch.matchAll(/^\+\+\+ (?:b\/)?(.+)$/gm)].map((match) => match[1]),
    ...[...patch.matchAll(/^rename (?:from|to) (.+)$/gm)].map((match) => match[1]),
  ].filter((path) => path !== "/dev/null");
  if (paths.length === 0) throw new Error("self-modification patch contains no stageable paths");
  return [...new Set(paths)];
}

function validateWorkflowAllowed(workflow: string): void {
  if (!/^[A-Za-z0-9._-]+\.ya?ml$/.test(workflow)) {
    throw new Error(`invalid workflow name: ${workflow}`);
  }
  const allowed = new Set(
    (process.env.PLOY_HARNESS_SELF_MOD_DEPLOY_WORKFLOWS || "")
      .split(",")
      .map((item) => item.trim())
      .filter(Boolean)
  );
  if (!allowed.has(workflow)) {
    throw new Error(`self-modification deployment workflow is not allowlisted: ${workflow}`);
  }
}

function validateWorkflowInputs(inputs: WorkflowInputs): void {
  for (const [key, value] of Object.entries(inputs)) {
    if (!/^[A-Za-z_][A-Za-z0-9_-]{0,63}$/.test(key)) {
      throw new Error(`invalid workflow input name: ${key}`);
    }
    if (!["string", "number", "boolean"].includes(typeof value)) {
      throw new Error(`invalid workflow input value for ${key}`);
    }
  }
}

function validateWorkflowRef(workflowRef: string, inputs: WorkflowInputs): void {
  if (process.env.PLOY_HARNESS_SELF_MOD_ALLOW_NON_MAIN_DEPLOY_REF === "true") return;
  if (workflowRef !== "main") {
    throw new Error("self-modification deployment dispatch requires workflow_ref=main");
  }
  const gitRef = inputs.git_ref;
  if (gitRef !== undefined && String(gitRef) !== "main") {
    throw new Error("self-modification deployment dispatch requires git_ref=main");
  }
}

async function dispatchWorkflow(
  cwd: string,
  workflow: string,
  workflowRef: string,
  inputs: WorkflowInputs
): Promise<string | undefined> {
  const args = ["workflow", "run", workflow, "--ref", workflowRef];
  for (const [key, value] of Object.entries(inputs)) {
    args.push("-f", `${key}=${String(value)}`);
  }
  await run(cwd, "gh", args);
  const runUrl = (
    await runOutput(cwd, "gh", [
      "run",
      "list",
      "--workflow",
      workflow,
      "--limit",
      "1",
      "--json",
      "url",
      "--jq",
      ".[0].url",
    ]).catch(() => "")
  ).trim();
  return runUrl || undefined;
}

async function recordApplied(
  proposal: SelfModificationProposal,
  updates: Partial<SelfModificationProposal>
): Promise<SelfModificationProposal> {
  const applied: SelfModificationProposal = {
    ...proposal,
    ...updates,
    status: "applied",
    applied_at: new Date().toISOString(),
  };
  await appendFile(await harnessSelfModificationsPath(), `${JSON.stringify(applied)}\n`, "utf8");
  return applied;
}

async function readProposal(proposalId: string): Promise<SelfModificationProposal> {
  const body = await readFile(await harnessSelfModificationsPath(), "utf8");
  for (const line of body.split(/\r?\n/)) {
    if (!line.trim()) continue;
    const proposal = JSON.parse(line) as SelfModificationProposal;
    if (proposal.proposal_id === proposalId && proposal.status === "proposed") {
      return proposal;
    }
  }
  throw new Error(`self-modification proposal ${proposalId} was not found`);
}

async function findRepoRoot(): Promise<string> {
  if (process.env.PLOY_SELF_MOD_REPO_ROOT) return resolve(process.env.PLOY_SELF_MOD_REPO_ROOT);
  let current = resolve(process.cwd());
  for (;;) {
    try {
      await readFile(join(current, "Cargo.toml"), "utf8");
      await readFile(join(current, ".git", "HEAD"), "utf8");
      return current;
    } catch {
      const parent = dirname(current);
      if (parent === current) throw new Error("could not locate repo root");
      current = parent;
    }
  }
}

async function ensureCleanWorktree(cwd: string): Promise<void> {
  const { stdout } = await execFileAsync("git", ["status", "--porcelain"], { cwd });
  if (stdout.trim()) {
    throw new Error("self-modification requires a clean git worktree");
  }
}

async function currentBranch(cwd: string): Promise<string> {
  const branch = (await runOutput(cwd, "git", ["branch", "--show-current"])).trim();
  if (!branch) throw new Error("self-modification PR publishing requires a named git branch");
  return branch;
}

async function runOutput(cwd: string, command: string, args: string[]): Promise<string> {
  const { stdout } = await execFileAsync(command, args, {
    cwd,
    maxBuffer: 1024 * 1024 * 10,
  });
  return stdout;
}

async function run(cwd: string, command: string, args: string[], input?: string): Promise<void> {
  if (input === undefined) {
    await execFileAsync(command, args, {
      cwd,
      maxBuffer: 1024 * 1024 * 10,
    });
    return;
  }
  await new Promise<void>((resolvePromise, reject) => {
    const child = spawn(command, args, { cwd, stdio: ["pipe", "pipe", "pipe"] });
    let stderr = "";
    child.stderr.on("data", (chunk) => {
      stderr += String(chunk);
    });
    child.on("error", reject);
    child.on("close", (code) => {
      if (code === 0) {
        resolvePromise();
      } else {
        reject(new Error(stderr || `${command} exited with ${code}`));
      }
    });
    child.stdin.end(input);
  });
}

async function selfTest() {
  const originalPath = process.env.PLOY_HARNESS_SELF_MODIFICATIONS_FILE;
  const originalRoot = process.env.PLOY_SELF_MOD_REPO_ROOT;
  const originalToken = process.env.PLOY_HARNESS_SELF_MOD_APPROVAL_TOKEN;
  const originalAllowPr = process.env.PLOY_HARNESS_SELF_MOD_ALLOW_PR;
  const originalAllowDeploy = process.env.PLOY_HARNESS_SELF_MOD_ALLOW_DEPLOY;
  const originalDeployWorkflows = process.env.PLOY_HARNESS_SELF_MOD_DEPLOY_WORKFLOWS;
  const originalNpmLog = process.env.PLOY_SELF_MOD_NPM_LOG;
  const originalPathValue = process.env.PATH;
  const dir = await mkdtemp(join(tmpdir(), "ploy-self-mod-"));
  const logDir = await mkdtemp(join(tmpdir(), "ploy-self-mod-log-"));
  const binDir = await mkdtemp(join(tmpdir(), "ploy-self-mod-bin-"));
  const remoteDir = await mkdtemp(join(tmpdir(), "ploy-self-mod-origin-"));
  const logPath = join(logDir, "self-mod.jsonl");
  const npmLogPath = join(logDir, "npm.log");

  try {
    process.env.PLOY_HARNESS_SELF_MODIFICATIONS_FILE = logPath;
    process.env.PLOY_SELF_MOD_REPO_ROOT = dir;
    process.env.PLOY_HARNESS_SELF_MOD_APPROVAL_TOKEN = "approve";
    process.env.PLOY_HARNESS_SELF_MOD_ALLOW_PR = "true";
    process.env.PLOY_HARNESS_SELF_MOD_ALLOW_DEPLOY = "true";
    process.env.PLOY_HARNESS_SELF_MOD_DEPLOY_WORKFLOWS = "deploy-test.yml,rollback-test.yml";
    process.env.PATH = `${binDir}${delimiter}${process.env.PATH || ""}`;
    await writeFile(
      join(binDir, "gh"),
      [
        "#!/bin/sh",
        "case \"$1 $2\" in",
        "  \"pr create\") printf '%s\\n' 'https://github.example/ploy/pull/1' ;;",
        "  \"workflow run\") exit 0 ;;",
        "  \"run list\") printf '%s\\n' 'https://github.example/ploy/actions/runs/42' ;;",
        "  *) exit 1 ;;",
        "esac",
        "",
      ].join("\n"),
      "utf8"
    );
    await chmod(join(binDir, "gh"), 0o755);
    process.env.PLOY_SELF_MOD_NPM_LOG = npmLogPath;
    await writeFile(
      join(binDir, "npm"),
      "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$PLOY_SELF_MOD_NPM_LOG\"\nexit 0\n",
      "utf8"
    );
    await chmod(join(binDir, "npm"), 0o755);
    await run(dir, "git", ["init"]);
    await run(dir, "git", ["checkout", "-b", "main"]);
    await run(dir, "git", ["config", "user.email", "test@example.com"]);
    await run(dir, "git", ["config", "user.name", "Test"]);
    await writeFile(join(dir, "Cargo.toml"), "[workspace]\n", "utf8");
    await writeFile(join(dir, "README.md"), "before\n", "utf8");
    await run(dir, "git", ["add", "."]);
    await run(dir, "git", ["commit", "-m", "init"]);
    await mkdir(remoteDir, { recursive: true });
    await run(remoteDir, "git", ["init", "--bare"]);
    await run(dir, "git", ["remote", "add", "origin", remoteDir]);
    await run(dir, "git", ["push", "-u", "origin", "main"]);

    await runVerificationProfile(dir, "sidecar_frontend_build");
    assert.deepEqual((await readFile(npmLogPath, "utf8")).trim().split(/\r?\n/), [
      "run build --prefix ploy-sidecar",
      "run build --prefix ploy-frontend",
    ], "sidecar_frontend_build_invokes_both_fixed_builds");
    await writeFile(npmLogPath, "", "utf8");

    const proposal = await proposeSelfModification({
      title: "update readme",
      patch: "diff --git a/README.md b/README.md\nindex 229a18c..13d29f8 100644\n--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-before\n+after\n",
      verification_command: "grep -q after README.md",
    });
    const applied = await applyApprovedSelfModification({
      proposal_id: proposal.proposal_id,
      approval_token: selfModificationApprovalProof(proposal, "approve"),
      publish_pull_request: true,
      branch_name: "selfmod/test",
    });
    assert.equal(applied.status, "applied");
    assert.equal(applied.branch_name, "selfmod/test");
    assert.match(applied.commit_sha ?? "", /^[0-9a-f]{40}$/);
    assert.equal(applied.pull_request_url, "https://github.example/ploy/pull/1");
    assert.equal(await readFile(join(dir, "README.md"), "utf8"), "after\n");
    assert.match(await readFile(logPath, "utf8"), /"status":"applied"/);
    assert.equal((await runOutput(dir, "git", ["branch", "--show-current"])).trim(), "selfmod/test");

    const failedProposal = await proposeSelfModification({
      title: "failing change",
      patch: "diff --git a/FAIL.md b/FAIL.md\nnew file mode 100644\nindex 0000000..257cc56\n--- /dev/null\n+++ b/FAIL.md\n@@ -0,0 +1 @@\n+fail\n",
      verification_command: "false",
    });
    await assert.rejects(
      applyApprovedSelfModification({
        proposal_id: failedProposal.proposal_id,
        approval_token: selfModificationApprovalProof(proposal, "approve"),
        publish_pull_request: true,
        branch_name: "selfmod/fail",
      })
    );
    assert.notEqual(
      selfModificationApprovalProof(proposal, "approve"),
      selfModificationApprovalProof(failedProposal, "approve"),
      "approval_proof_cannot_be_reused_for_another_proposal"
    );
    await assert.rejects(
      async () => verifySelfModificationProposalApproval(
        { ...proposal, patch: `${proposal.patch}\n# tampered` },
        selfModificationApprovalProof(proposal, "approve")
      ),
      /patch hash mismatch/,
      "tampered_patch_with_old_hash_and_proof_is_rejected"
    );
    assert.equal((await runOutput(dir, "git", ["branch", "--show-current"])).trim(), "selfmod/test");
    assert.equal((await runOutput(dir, "git", ["branch", "--list", "selfmod/fail"])).trim(), "");

    await writeFile(join(dir, "DELETE.md"), "delete me\n", "utf8");
    await writeFile(join(dir, "RENAME-OLD.md"), "rename me\n", "utf8");
    await run(dir, "git", ["add", "--", "DELETE.md", "RENAME-OLD.md"]);
    await run(dir, "git", ["commit", "-m", "self-test patch paths"]);

    await rm(join(dir, "DELETE.md"));
    const deletionPatch = await runOutput(dir, "git", ["diff", "--", "DELETE.md"]);
    await writeFile(join(dir, "DELETE.md"), "delete me\n", "utf8");
    await run(dir, "git", ["apply", "-"], deletionPatch);
    await run(dir, "git", ["add", "--", ...patchPaths(deletionPatch)]);
    assert.equal(await runOutput(dir, "git", ["diff", "--cached"]), deletionPatch,
      "delete_only_patch_stages_deleted_path");
    await run(dir, "git", ["restore", "--staged", "--", "DELETE.md"]);
    await writeFile(join(dir, "DELETE.md"), "delete me\n", "utf8");

    await rename(join(dir, "RENAME-OLD.md"), join(dir, "RENAME-NEW.md"));
    await run(dir, "git", ["add", "--", "RENAME-OLD.md", "RENAME-NEW.md"]);
    const renamePatch = await runOutput(dir, "git", ["diff", "--cached", "--find-renames"]);
    await run(dir, "git", ["restore", "--staged", "--", "RENAME-OLD.md", "RENAME-NEW.md"]);
    await rename(join(dir, "RENAME-NEW.md"), join(dir, "RENAME-OLD.md"));
    await run(dir, "git", ["apply", "-"], renamePatch);
    await run(dir, "git", ["add", "--", ...patchPaths(renamePatch)]);
    assert.equal(await runOutput(dir, "git", ["diff", "--cached", "--find-renames"]), renamePatch,
      "rename_patch_stages_old_and_new_paths");
    await run(dir, "git", ["restore", "--staged", "--", "RENAME-OLD.md", "RENAME-NEW.md"]);
    await rm(join(dir, "RENAME-NEW.md"));
    await writeFile(join(dir, "RENAME-OLD.md"), "rename me\n", "utf8");

    const deployment = await dispatchApprovedSelfModificationDeployment({
      approval_token: "approve",
      workflow: "deploy-test.yml",
      inputs: { git_ref: "main", deploy: true },
      rollback_workflow: "rollback-test.yml",
      rollback_inputs: { git_ref: "main", deploy: true },
    });
    assert.equal(deployment.workflow, "deploy-test.yml");
    assert.equal(deployment.rollback_workflow, "rollback-test.yml");
    assert.equal(deployment.run_url, "https://github.example/ploy/actions/runs/42");
    await assert.rejects(
      dispatchApprovedSelfModificationDeployment({
        approval_token: "approve",
        workflow: "not-allowed.yml",
        inputs: { git_ref: "main" },
        rollback_workflow: "rollback-test.yml",
      })
    );

    const rollback = await dispatchApprovedSelfModificationRollback({
      approval_token: "approve",
      workflow: "rollback-test.yml",
      inputs: { git_ref: "main", deploy: true },
    });
    assert.equal(rollback.workflow, "rollback-test.yml");
    assert.equal(rollback.run_url, "https://github.example/ploy/actions/runs/42");
    assert.match(await readFile(logPath, "utf8"), /self_modification_deployment_dispatch/);
    assert.match(await readFile(logPath, "utf8"), /self_modification_rollback_dispatch/);
  } finally {
    restoreEnv("PLOY_HARNESS_SELF_MODIFICATIONS_FILE", originalPath);
    restoreEnv("PLOY_SELF_MOD_REPO_ROOT", originalRoot);
    restoreEnv("PLOY_HARNESS_SELF_MOD_APPROVAL_TOKEN", originalToken);
    restoreEnv("PLOY_HARNESS_SELF_MOD_ALLOW_PR", originalAllowPr);
    restoreEnv("PLOY_HARNESS_SELF_MOD_ALLOW_DEPLOY", originalAllowDeploy);
    restoreEnv("PLOY_HARNESS_SELF_MOD_DEPLOY_WORKFLOWS", originalDeployWorkflows);
    restoreEnv("PLOY_SELF_MOD_NPM_LOG", originalNpmLog);
    restoreEnv("PATH", originalPathValue);
    await rm(dir, { recursive: true, force: true });
    await rm(logDir, { recursive: true, force: true });
    await rm(binDir, { recursive: true, force: true });
    await rm(remoteDir, { recursive: true, force: true });
  }
}

function restoreEnv(key: string, value: string | undefined) {
  if (value === undefined) {
    delete process.env[key];
  } else {
    process.env[key] = value;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await selfTest();
}
