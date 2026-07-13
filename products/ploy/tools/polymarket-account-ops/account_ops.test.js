"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");
const {
  CHAIN_ID,
  CONTRACTS,
  buildPlan,
  executePlan,
  fetchPositions,
  readLedger,
  relayerWalletRoute,
  reconcileOperation,
  reconcileTransaction,
  redeemTransaction,
  sha256,
  validatePlan,
} = require("./account_ops");

const account = "0x1111111111111111111111111111111111111111";
const releaseSha = "a".repeat(40);
const now = Date.parse("2026-07-12T12:00:00Z");

function position(overrides = {}) {
  return {
    proxyWallet: account,
    asset: "123",
    conditionId: `0x${"2".repeat(64)}`,
    size: "10.5",
    redeemable: true,
    negativeRisk: false,
    outcome: "Yes",
    outcomeIndex: 0,
    ...overrides,
  };
}

test("plan groups a condition and binds account, chain, release, expiry, and hash", () => {
  const plan = buildPlan(
    [position(), position({ asset: "456", outcome: "No", outcomeIndex: 1 })],
    { account, releaseSha, walletType: "SAFE" },
    now,
  );
  assert.equal(plan.chainId, CHAIN_ID);
  assert.equal(plan.account.toLowerCase(), account);
  assert.equal(plan.items.length, 1);
  assert.equal(plan.items[0].target, CONTRACTS.standardAdapter);
  assert.equal(Date.parse(plan.expiresAt) - Date.parse(plan.generatedAt), 10 * 60 * 1000);
  assert.equal(validatePlan(plan, { account, releaseSha, walletType: "SAFE" }, plan.sha256, now), plan);
});

test("validation rejects stale plans and account drift", () => {
  const plan = buildPlan([position()], { account, releaseSha, walletType: "PROXY" }, now);
  assert.throws(
    () => validatePlan(plan, { account, releaseSha, walletType: "PROXY" }, plan.sha256, now + 10 * 60 * 1000),
    /expired/,
  );
  assert.throws(
    () => validatePlan(plan, { account: "0x3333333333333333333333333333333333333333", releaseSha, walletType: "PROXY" }, plan.sha256, now),
    /account mismatch/,
  );
});

test("redeem calldata always routes through the current collateral adapter", () => {
  assert.equal(CONTRACTS.standardAdapter, "0xAdA100Db00Ca00073811820692005400218FcE1f");
  assert.equal(CONTRACTS.negRiskAdapter, "0xadA2005600Dec949baf300f4C6120000bDB6eAab");
  const plan = buildPlan([position({ negativeRisk: true })], { account, releaseSha, walletType: "SAFE" }, now);
  const transaction = redeemTransaction(plan.items[0]);
  assert.equal(transaction.to, CONTRACTS.negRiskAdapter);
  assert.equal(transaction.value, "0");
  assert.ok(transaction.data.startsWith("0x"));
});

test("validation rejects route and operation identity drift even with a recomputed plan hash", () => {
  const plan = buildPlan([position()], { account, releaseSha, walletType: "SAFE" }, now);
  plan.items[0].route = "unsupported";
  const { sha256: _oldHash, ...unsigned } = plan;
  plan.sha256 = sha256(unsigned);
  assert.throws(
    () => validatePlan(plan, { account, releaseSha, walletType: "SAFE" }, plan.sha256, now),
    /unsupported redeem route/,
  );
});

test("position discovery follows Data API offset pagination", async () => {
  const offsets = [];
  const positions = await fetchPositions(account, async (url) => {
    offsets.push(url.searchParams.get("offset"));
    const offset = Number(url.searchParams.get("offset"));
    return {
      ok: true,
      json: async () => offset === 0
        ? Array.from({ length: 500 }, (_, index) => ({ asset: String(index) }))
        : [{ asset: "last" }],
    };
  });
  assert.deepEqual(offsets, ["0", "500"]);
  assert.equal(positions.length, 501);
});

test("plan fails closed on wallet and route disagreement", () => {
  assert.throws(
    () => buildPlan([position({ proxyWallet: "0x3333333333333333333333333333333333333333" })], { account, releaseSha, walletType: "SAFE" }, now),
    /wallet does not match/,
  );
  assert.throws(
    () => buildPlan([position(), position({ negativeRisk: true })], { account, releaseSha, walletType: "SAFE" }, now),
    /negative-risk disagreement/,
  );
});

test("custody relayer rejects unsupported deposit/poly1271 wallets", () => {
  assert.throws(() => relayerWalletRoute("DEPOSIT"), /poly1271 custody is not supported/);
  assert.throws(() => relayerWalletRoute("unknown"), /walletType must be SAFE or PROXY/);
});

test("execute refuses to enter account operations while writes are disabled", async () => {
  await assert.rejects(() => executePlan({}, "ignored", {}), /disabled inside Monday/);
});

test("Monday rejects every account-op mutation even when legacy write flags are set", async () => {
  const env = { PLOY_ACCOUNT_OPS_WRITE_ENABLED: "true" };
  const dependencies = new Proxy({}, {
    get() { throw new Error("legacy dependency must not be reached"); },
  });

  await assert.rejects(() => executePlan({}, "ignored", env, dependencies), /disabled inside Monday/);
  await assert.rejects(() => reconcileOperation("operation", env, dependencies), /disabled inside Monday/);
  await assert.rejects(() => reconcileTransaction("transaction", env, dependencies), /disabled inside Monday/);
});

test.skip("historical standalone execution retained a lock after an unknown relayer response", async (t) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "ploy-redeem-submit-unknown-"));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const ledger = path.join(directory, "redeem-ledger.jsonl");
  const env = {
    PLOY_ACCOUNT_OPS_WRITE_ENABLED: "true",
    PLOY_LIVE_ACCOUNT_ID: account,
    PLOY_RELEASE_SHA: releaseSha,
    POLY_WALLET_TYPE: "SAFE",
    PLOY_REDEEM_LEDGER: ledger,
    PLOY_REDEEM_LOCK: `${ledger}.lock`,
  };
  const plan = buildPlan([position()], { account, releaseSha, walletType: "SAFE" });
  await assert.rejects(
    () => executePlan(plan, plan.sha256, env, {
      relay: {
        client: { execute: async () => { throw new Error("connection reset"); } },
        publicClient: {
          readContract: async ({ functionName }) => functionName === "isApprovedForAll" ? true : 100n,
        },
      },
      fetchPositions: async () => [position()],
      verifyPositionRoutes: async (positions) => positions,
    }),
    /submission outcome is unknown/,
  );
  assert.equal(fs.existsSync(`${ledger}.lock`), true);
  assert.equal(readLedger(ledger).at(-1).state, "submission_unknown");
});

test.skip("historical standalone reconciliation discovered an exact relayer transaction", async (t) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "ploy-redeem-operation-"));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const ledger = path.join(directory, "redeem-ledger.jsonl");
  const lock = `${ledger}.lock`;
  fs.mkdirSync(lock);
  const conditionId = position().conditionId;
  const operationId = "operation-discovery";
  const at = new Date().toISOString();
  fs.writeFileSync(ledger, [
    { at, operationId, conditionId, state: "submitting", metadata: `ploy redeem ${conditionId}` },
    { at, operationId, conditionId, state: "submission_unknown" },
  ].map(JSON.stringify).join("\n") + "\n");
  const transaction = {
    transactionID: "relay-discovered",
    transactionHash: "0xabc",
    proxyAddress: account,
    metadata: `ploy redeem ${conditionId}`,
    createdAt: at,
    state: "STATE_MINED",
  };
  const relay = {
    client: {
      getTransactions: async () => [
        { ...transaction, transactionID: "missing-account", proxyAddress: undefined },
        { ...transaction, transactionID: "too-late", createdAt: new Date(Date.parse(at) + 11 * 60 * 1000).toISOString() },
        transaction,
      ],
      getTransaction: async () => [transaction],
    },
    publicClient: { readContract: async () => 150n },
  };
  const result = await reconcileOperation(operationId, {
    PLOY_LIVE_ACCOUNT_ID: account,
    PLOY_RELEASE_SHA: releaseSha,
    POLY_WALLET_TYPE: "SAFE",
    PLOY_REDEEM_LEDGER: ledger,
    PLOY_REDEEM_LOCK: lock,
  }, {
    relay,
    fetchPositions: async () => [],
    verifyPositionRoutes: async (positions) => positions,
  });
  assert.equal(result.state, "reconciled");
  assert.equal(fs.existsSync(lock), false);
  assert.equal(readLedger(ledger).some((event) => event.discoveredByOperation), true);
});

test.skip("historical standalone reconciliation cleared a settled transaction lock", async (t) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "ploy-redeem-reconcile-"));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const ledger = path.join(directory, "redeem-ledger.jsonl");
  const lock = `${ledger}.lock`;
  fs.mkdirSync(lock);
  fs.writeFileSync(ledger, `${JSON.stringify({
    operationId: "operation-1",
    conditionId: position().conditionId,
    state: "ambiguous",
    transactionId: "relay-1",
  })}\n`);
  const env = {
    PLOY_LIVE_ACCOUNT_ID: account,
    PLOY_RELEASE_SHA: releaseSha,
    POLY_WALLET_TYPE: "SAFE",
    PLOY_REDEEM_LEDGER: ledger,
    PLOY_REDEEM_LOCK: lock,
  };
  const result = await reconcileTransaction("relay-1", env, {
    relay: {
      client: { getTransaction: async () => [{ transactionID: "relay-1", transactionHash: "0xabc", state: "STATE_MINED" }] },
      publicClient: { readContract: async () => 125n },
    },
    fetchPositions: async () => [],
    verifyPositionRoutes: async (positions) => positions,
  });
  assert.equal(result.state, "reconciled");
  assert.equal(fs.existsSync(lock), false);
  assert.equal(readLedger(ledger).at(-1).balanceAfter, "125");
});

test.skip("historical standalone reconciliation retained a pending transaction lock", async (t) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "ploy-redeem-pending-"));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const ledger = path.join(directory, "redeem-ledger.jsonl");
  const lock = `${ledger}.lock`;
  fs.mkdirSync(lock);
  fs.writeFileSync(ledger, `${JSON.stringify({
    operationId: "operation-2",
    conditionId: position().conditionId,
    state: "submitted",
    transactionId: "relay-2",
  })}\n`);
  const env = {
    PLOY_LIVE_ACCOUNT_ID: account,
    PLOY_RELEASE_SHA: releaseSha,
    POLY_WALLET_TYPE: "SAFE",
    PLOY_REDEEM_LEDGER: ledger,
    PLOY_REDEEM_LOCK: lock,
  };
  await assert.rejects(
    () => reconcileTransaction("relay-2", env, {
      relay: { client: { getTransaction: async () => [{ transactionID: "relay-2", state: "STATE_EXECUTED" }] } },
    }),
    /still STATE_EXECUTED/,
  );
  assert.equal(fs.existsSync(lock), true);
  assert.equal(readLedger(ledger).at(-1).state, "ambiguous");
});
