"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");
const {
  buildOrderPlan,
  buildRedeemPlan,
  executeApprovals,
  executeOrderPlan,
  executeRedeemPlan,
  loadWalletSecret,
  makeWalletSession,
  reconcileRedeem,
  sha256,
  validatePlan,
} = require("./account_ops");

const MARKET = {
  id: 42,
  conditionId: `0x${"ab".repeat(32)}`,
  decimalPrecision: 3,
  feeRateBps: 200,
  isNegRisk: true,
  isYieldBearing: true,
  tradingStatus: "OPEN",
  status: "REGISTERED",
  isVisible: true,
  resolution: null,
  outcomes: [
    { name: "Yes", indexSet: 1, onChainId: "111", status: null },
    { name: "No", indexSet: 2, onChainId: "222", status: null },
  ],
};
const CONTEXT = {
  account: "0x1111111111111111111111111111111111111111",
  accountType: "PREDICT_ACCOUNT",
  chainId: 56,
  releaseSha: "a".repeat(40),
};

test("order plan binds official market route, account, expiry, and exact hash", () => {
  const plan = buildOrderPlan(MARKET, {
    marketId: 42,
    tokenId: "111",
    side: "BUY",
    quantity: "10",
    limitPrice: "0.425",
  }, CONTEXT, 1_700_000_000_000);

  assert.equal(plan.kind, "predict_fun_limit_order");
  assert.equal(plan.market.isNegRisk, true);
  assert.equal(plan.order.feeRateBps, 200);
  assert.equal(plan.order.outcomeStatus, "null");
  assert.equal(plan.order.pricePerShareWei, "425000000000000000");
  assert.equal(plan.order.makerAmount, "4250000000000000000");
  assert.equal(plan.order.takerAmount, "10000000000000000000");
  assert.equal(plan.expiresAt, "2023-11-14T22:23:20.000Z");
  assert.equal(validatePlan(plan, CONTEXT, plan.sha256, 1_700_000_001_000), plan);
  assert.throws(() => validatePlan(plan, CONTEXT, "0".repeat(64)), /plan hash mismatch/);
  const invalidTime = { ...plan, generatedAt: "not-a-date" };
  const { sha256: _oldHash, ...unsigned } = invalidTime;
  invalidTime.sha256 = sha256(unsigned);
  assert.throws(() => validatePlan(invalidTime, CONTEXT, invalidTime.sha256), /timestamps are invalid/);
});

test("order plan rejects a token or price not supported by the market", () => {
  assert.throws(() => buildOrderPlan(MARKET, {
    marketId: 42, tokenId: "999", side: "BUY", quantity: "1", limitPrice: "0.5",
  }, CONTEXT), /tokenId is not an outcome/);
  assert.throws(() => buildOrderPlan(MARKET, {
    marketId: 42, tokenId: "111", side: "BUY", quantity: "1", limitPrice: "0.4251",
  }, CONTEXT), /decimal precision/);
  assert.throws(() => buildOrderPlan({ ...MARKET, decimalPrecision: 4 }, {
    marketId: 42, tokenId: "111", side: "SELL", quantity: "1", limitPrice: "0.1234",
  }, CONTEXT), /truncated/);
  assert.throws(() => buildOrderPlan({ ...MARKET, tradingStatus: "CLOSED" }, {
    marketId: 42, tokenId: "111", side: "BUY", quantity: "1", limitPrice: "0.5",
  }, CONTEXT), /registered, visible, open, and unresolved/);
  assert.throws(() => buildOrderPlan({ ...MARKET, outcomes: [{ ...MARKET.outcomes[0], status: "WON" }] }, {
    marketId: 42, tokenId: "111", side: "BUY", quantity: "1", limitPrice: "0.5",
  }, CONTEXT), /outcome must be unresolved/);
});

test("redeem plan keeps standard and neg-risk amount semantics distinct", () => {
  const positions = [{
    amount: "2500000000000000000",
    market: { ...MARKET, resolution: { indexSet: 1, status: "WON" } },
    outcome: MARKET.outcomes[0],
  }];
  const plan = buildRedeemPlan(positions, CONTEXT, 1_700_000_000_000);
  assert.equal(plan.items.length, 1);
  assert.equal(plan.items[0].amount, "2500000000000000000");
  assert.equal(plan.items[0].isNegRisk, true);
  assert.equal(plan.items[0].indexSet, 1);
});

test("wallet secret requires exactly one injected source and never accepts loose files", () => {
  assert.throws(() => loadWalletSecret({}), /exactly one/);
  assert.throws(() => loadWalletSecret({
    PREDICT_FUN_PRIVATE_KEY: `0x${"11".repeat(32)}`,
    PREDICT_FUN_PRIVATE_KEY_FILE: "/tmp/key",
  }), /exactly one/);
});

test("wallet session rejects non-HTTPS RPC before loading a signer", async () => {
  await assert.rejects(makeWalletSession(CONTEXT, {
    PREDICT_FUN_RPC_URL: "http://rpc.invalid",
  }), /must use HTTPS/);
});

function runtimeEnv(tmp) {
  return {
    PLOY_LIVE_ACCOUNT_ID: CONTEXT.account,
    PREDICT_FUN_ACCOUNT_TYPE: CONTEXT.accountType,
    PREDICT_FUN_CHAIN_ID: String(CONTEXT.chainId),
    PLOY_RELEASE_SHA: CONTEXT.releaseSha,
    PLOY_PREDICT_OPS_LEDGER: path.join(tmp, "ledger.jsonl"),
  };
}

function fakeOrderSession() {
  return { builder: {
    getApprovalSteps: () => [{ id: "allowance" }],
    checkApprovals: async () => [{ satisfied: true }],
    getLimitOrderAmounts: ({ side, pricePerShareWei, quantityWei }) => ({
      makerAmount: side === 0 ? (pricePerShareWei * quantityWei) / 10n ** 18n : quantityWei,
      takerAmount: side === 0 ? quantityWei : (pricePerShareWei * quantityWei) / 10n ** 18n,
      pricePerShare: pricePerShareWei,
    }),
    buildOrder: (_strategy, order) => ({ ...order, salt: "1", maker: CONTEXT.account, signer: CONTEXT.account }),
    buildTypedData: (order) => ({ message: order }),
    signTypedDataOrder: async ({ message }) => ({ ...message, signature: "0xsig" }),
    buildTypedDataHash: () => `0x${"cd".repeat(32)}`,
  } };
}

test("order execution is write-gated and submits one official SDK payload", async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "predict-order-"));
  const plan = buildOrderPlan(MARKET, {
    marketId: 42, tokenId: "111", side: "BUY", quantity: "10", limitPrice: "0.425",
  }, CONTEXT);
  await assert.rejects(executeOrderPlan(plan, plan.sha256, runtimeEnv(tmp)), /writes are disabled/);
  let submitted;
  const events = [];
  const result = await executeOrderPlan(plan, plan.sha256, {
    ...runtimeEnv(tmp), PLOY_PREDICT_ACCOUNT_OPS_WRITE_ENABLED: "true",
  }, {
    market: MARKET,
    session: fakeOrderSession(),
    jwt: "jwt",
    record: (event) => events.push(event),
    submit: async (body) => { submitted = body; return { data: { orderId: "order-1", orderHash: body.data.order.hash } }; },
  });
  assert.equal(result.orderId, "order-1");
  assert.equal(submitted.data.strategy, "LIMIT");
  assert.equal(submitted.data.order.signature, "0xsig");
  assert.deepEqual(events.map((event) => event.state), ["submitting", "submitted"]);
});

test("order execution rechecks expiry immediately before submission", async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "predict-expiry-"));
  const generatedAt = 1_700_000_000_000;
  const plan = buildOrderPlan(MARKET, {
    marketId: 42, tokenId: "111", side: "BUY", quantity: "10", limitPrice: "0.425",
  }, CONTEXT, generatedAt);
  const times = [generatedAt + 1, generatedAt + 2, generatedAt + 600_001];
  let submitted = false;
  await assert.rejects(executeOrderPlan(plan, plan.sha256, {
    ...runtimeEnv(tmp), PLOY_PREDICT_ACCOUNT_OPS_WRITE_ENABLED: "true",
  }, {
    market: MARKET,
    session: fakeOrderSession(),
    jwt: "jwt",
    now: () => times.shift(),
    submit: async () => { submitted = true; return { data: {} }; },
  }), /expired/);
  assert.equal(submitted, false);
  assert.equal(fs.existsSync(path.join(tmp, "ledger.jsonl.lock")), false);
});

test("approval execution rechecks TTL before every approval transaction", async () => {
  const generatedAt = 1_700_000_000_000;
  const plan = buildOrderPlan(MARKET, {
    marketId: 42, tokenId: "111", side: "BUY", quantity: "10", limitPrice: "0.425",
  }, CONTEXT, generatedAt);
  const times = [generatedAt + 1, generatedAt + 2, generatedAt + 600_001];
  const submitted = [];
  const session = { builder: {
    getApprovalSteps: () => [{ id: "one" }, { id: "two" }],
    checkApprovals: async ([step]) => [{ step, satisfied: false }],
    setApproval: async (step) => {
      submitted.push(step.id);
      return { success: true, receipt: { status: 1 } };
    },
  } };
  await assert.rejects(executeApprovals(plan, plan.sha256, {
    ...runtimeEnv(os.tmpdir()), PLOY_PREDICT_APPROVAL_WRITE_ENABLED: "true",
  }, { session, now: () => times.shift() }), /expired/);
  assert.deepEqual(submitted, ["one"]);
});

test("order execution rejects outcome metadata drift before signing", async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "predict-outcome-drift-"));
  const plan = buildOrderPlan(MARKET, {
    marketId: 42, tokenId: "111", side: "BUY", quantity: "10", limitPrice: "0.425",
  }, CONTEXT);
  const changed = {
    ...MARKET,
    outcomes: [{ ...MARKET.outcomes[0], indexSet: 2 }, MARKET.outcomes[1]],
  };
  await assert.rejects(executeOrderPlan(plan, plan.sha256, {
    ...runtimeEnv(tmp), PLOY_PREDICT_ACCOUNT_OPS_WRITE_ENABLED: "true",
  }, { market: changed, session: fakeOrderSession(), jwt: "jwt" }), /outcome metadata changed/);
});

test("redemption execution routes neg-risk amount through official SDK and records receipt", async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "predict-redeem-"));
  const positions = [{
    amount: "2500000000000000000",
    market: { ...MARKET, resolution: { indexSet: 1, status: "WON" } },
    outcome: MARKET.outcomes[0],
  }];
  const plan = buildRedeemPlan(positions, CONTEXT);
  let options;
  const events = [];
  const session = { builder: {
    getApprovalSteps: () => [{ id: "neg-risk-adapter" }],
    checkApprovals: async () => [{ satisfied: true }],
    redeemPositions: async (value) => { options = value; return { success: true, receipt: { status: 1, hash: "0xtx" } }; },
  } };
  const result = await executeRedeemPlan(plan, plan.sha256, {
    ...runtimeEnv(tmp), PLOY_PREDICT_ACCOUNT_OPS_WRITE_ENABLED: "true",
  }, { positions, session, record: (event) => events.push(event) });
  assert.equal(options.amount, 2500000000000000000n);
  assert.equal(options.isNegRisk, true);
  assert.equal(result[0].transactionHash, "0xtx");
  assert.deepEqual(events.map((event) => event.state), ["submitting", "confirmed"]);
});

test("confirmed redemption retains its owner lock when ledger persistence fails", async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "predict-ledger-failure-"));
  const positions = [{
    amount: "2500000000000000000",
    market: { ...MARKET, resolution: { indexSet: 1, status: "WON" } },
    outcome: MARKET.outcomes[0],
  }];
  const plan = buildRedeemPlan(positions, CONTEXT);
  const session = { builder: {
    getApprovalSteps: () => [],
    checkApprovals: async () => [],
    redeemPositions: async () => ({ success: true, receipt: { status: 1, hash: "0xtx" } }),
  } };
  await assert.rejects(executeRedeemPlan(plan, plan.sha256, {
    ...runtimeEnv(tmp), PLOY_PREDICT_ACCOUNT_OPS_WRITE_ENABLED: "true",
  }, {
    positions,
    session,
    record: (event) => { if (event.state === "confirmed") throw new Error("disk full"); },
  }), /disk full/);
  assert.equal(fs.existsSync(path.join(tmp, "ledger.jsonl.lock", "owner.json")), true);
});

test("redemption reconciliation only clears the lock owned by an exact settled plan", async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "predict-reconcile-"));
  const positions = [{
    amount: "2500000000000000000",
    market: { ...MARKET, resolution: { indexSet: 1, status: "WON" } },
    outcome: MARKET.outcomes[0],
  }];
  const plan = buildRedeemPlan(positions, CONTEXT, 1_700_000_000_000);
  const operationId = sha256({
    kind: plan.kind, account: plan.account, chainId: plan.chainId, planSha256: plan.sha256,
  });
  const lock = path.join(tmp, "ledger.jsonl.lock");
  fs.mkdirSync(lock);
  fs.writeFileSync(path.join(lock, "owner.json"), JSON.stringify({ operationId }));
  const env = {
    ...runtimeEnv(tmp),
    PLOY_PREDICT_RECONCILE_WRITE_ENABLED: "true",
  };
  await assert.rejects(reconcileRedeem(plan, plan.sha256, env, {
    positions,
    now: () => 1_700_001_000_000,
  }), /still redeemable/);
  assert.equal(fs.existsSync(lock), true);
  const result = await reconcileRedeem(plan, plan.sha256, env, {
    positions: [],
    now: () => 1_700_001_000_000,
    balanceOf: async () => 0n,
  });
  assert.equal(result.state, "reconciled");
  assert.equal(fs.existsSync(lock), false);
});
