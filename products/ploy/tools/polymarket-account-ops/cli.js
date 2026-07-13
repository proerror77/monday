#!/usr/bin/env node
"use strict";

const fs = require("node:fs");
const {
  buildPlan,
  executePlan,
  fetchPositions,
  reconcileOperation,
  reconcileTransaction,
  runtimeContext,
  verifyPositionRoutes,
} = require("./account_ops");

function option(name) {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : undefined;
}

async function main() {
  const command = process.argv[2] || "check";
  const context = runtimeContext();
  if (command === "check") {
    const positions = await verifyPositionRoutes(await fetchPositions(context.account));
    const redeemable = positions.filter((position) => position.redeemable && Number(position.size) > 0);
    process.stdout.write(`${JSON.stringify({ account: context.account, redeemable }, null, 2)}\n`);
    return;
  }
  if (command === "plan") {
    const output = option("--out");
    if (!output) throw new Error("plan requires --out <file>");
    const plan = buildPlan(await verifyPositionRoutes(await fetchPositions(context.account)), context);
    fs.writeFileSync(output, `${JSON.stringify(plan, null, 2)}\n`, { mode: 0o600, flag: "wx" });
    process.stdout.write(`${JSON.stringify({ path: output, sha256: plan.sha256, items: plan.items.length })}\n`);
    return;
  }
  if (command === "execute") {
    const planPath = option("--plan");
    const expectedHash = option("--sha256");
    if (!planPath || !expectedHash) throw new Error("execute requires --plan <file> --sha256 <hash>");
    await executePlan(JSON.parse(fs.readFileSync(planPath, "utf8")), expectedHash);
    process.stdout.write(`${JSON.stringify({ status: "reconciled" })}\n`);
    return;
  }
  if (command === "reconcile") {
    const transactionId = option("--transaction-id");
    const operationId = option("--operation-id");
    if (Boolean(transactionId) === Boolean(operationId)) {
      throw new Error("reconcile requires exactly one of --transaction-id <id> or --operation-id <id>");
    }
    const result = transactionId
      ? await reconcileTransaction(transactionId)
      : await reconcileOperation(operationId);
    process.stdout.write(`${JSON.stringify(result)}\n`);
    return;
  }
  throw new Error(`unsupported command: ${command}`);
}

main().catch((error) => {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
});
