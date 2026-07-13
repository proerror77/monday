#!/usr/bin/env node
"use strict";

const fs = require("node:fs");
const {
  buildOrderPlan,
  buildRedeemPlan,
  checkApprovals,
  executeApprovals,
  executeOrderPlan,
  executeRedeemPlan,
  fetchMarket,
  fetchPositions,
  makeWalletSession,
  reconcileOrder,
  reconcileRedeem,
  runtimeContext,
  validatePlan,
} = require("./account_ops");

function option(name) {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : undefined;
}

function requireOption(name) {
  const value = option(name);
  if (!value) throw new Error(`${name} is required`);
  return value;
}

function writePlan(output, plan) {
  fs.writeFileSync(output, `${json(plan, 2)}\n`, { mode: 0o600, flag: "wx" });
  process.stdout.write(`${json({ path: output, sha256: plan.sha256, kind: plan.kind })}\n`);
}

function json(value, space) {
  return JSON.stringify(value, (_key, item) => typeof item === "bigint" ? item.toString() : item, space);
}

function readPlan() {
  return JSON.parse(fs.readFileSync(requireOption("--plan"), "utf8"));
}

async function main() {
  const resource = process.argv[2];
  const action = process.argv[3];
  if (!resource || !action) {
    throw new Error("usage: ploy-predict-account-ops wallet check | order plan|approval-check|approve|execute|reconcile | redeem check|plan|approval-check|approve|execute|reconcile");
  }
  const context = runtimeContext();

  if (resource === "wallet" && action === "check") {
    await makeWalletSession(context);
    process.stdout.write(`${json({ account: context.account, accountType: context.accountType, chainId: context.chainId })}\n`);
    return;
  }
  if (resource === "order" && action === "plan") {
    const marketId = Number(requireOption("--market-id"));
    const market = await fetchMarket(marketId, context);
    writePlan(requireOption("--out"), buildOrderPlan(market, {
      marketId,
      tokenId: requireOption("--token-id"),
      side: requireOption("--side"),
      quantity: requireOption("--quantity"),
      limitPrice: requireOption("--limit-price"),
    }, context));
    return;
  }
  if (resource === "redeem" && action === "check") {
    const plan = buildRedeemPlan(await fetchPositions(context), context);
    process.stdout.write(`${json({ account: context.account, items: plan.items }, 2)}\n`);
    return;
  }
  if (resource === "redeem" && action === "plan") {
    writePlan(requireOption("--out"), buildRedeemPlan(await fetchPositions(context), context));
    return;
  }
  if (new Set(["order", "redeem"]).has(resource) && action === "approval-check") {
    const plan = readPlan();
    validatePlan(plan, context, plan.sha256);
    const session = await makeWalletSession(context);
    const report = await checkApprovals(plan, session);
    process.stdout.write(`${json(report, 2)}\n`);
    return;
  }
  if (new Set(["order", "redeem"]).has(resource) && action === "approve") {
    const plan = readPlan();
    const report = await executeApprovals(plan, requireOption("--sha256"));
    process.stdout.write(`${json(report, 2)}\n`);
    return;
  }
  if (resource === "order" && action === "execute") {
    const result = await executeOrderPlan(readPlan(), requireOption("--sha256"));
    process.stdout.write(`${json(result)}\n`);
    return;
  }
  if (resource === "order" && action === "reconcile") {
    const result = await reconcileOrder(readPlan(), requireOption("--sha256"));
    process.stdout.write(`${json(result)}\n`);
    return;
  }
  if (resource === "redeem" && action === "execute") {
    const result = await executeRedeemPlan(readPlan(), requireOption("--sha256"));
    process.stdout.write(`${json({ status: "confirmed", receipts: result })}\n`);
    return;
  }
  if (resource === "redeem" && action === "reconcile") {
    const result = await reconcileRedeem(readPlan(), requireOption("--sha256"));
    process.stdout.write(`${json(result)}\n`);
    return;
  }
  throw new Error("usage: ploy-predict-account-ops wallet check | order plan|approval-check|approve|execute|reconcile | redeem check|plan|approval-check|approve|execute|reconcile");
}

main().catch((error) => {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
});
