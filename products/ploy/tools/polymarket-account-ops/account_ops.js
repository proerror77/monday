"use strict";

const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const {
  RelayClient,
  RelayerTxType,
  deriveProxyWallet,
  deriveSafe,
} = require("@polymarket/builder-relayer-client");
const { BuilderConfig } = require("@polymarket/builder-signing-sdk");
const {
  createPublicClient,
  createWalletClient,
  encodeFunctionData,
  getAddress,
  http,
  zeroHash,
} = require("viem");
const { privateKeyToAccount } = require("viem/accounts");
const { polygon } = require("viem/chains");

const CHAIN_ID = 137;
const CONTRACTS = Object.freeze({
  pusd: "0xC011a7E12a19f7B1f670d46F03B03f3342E82DFB",
  conditionalTokens: "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045",
  standardAdapter: "0xAdA100Db00Ca00073811820692005400218FcE1f",
  negRiskAdapter: "0xadA2005600Dec949baf300f4C6120000bDB6eAab",
  safeFactory: "0xaacFeEa03eb1561C4e67d661e40682Bd20E3541b",
  proxyFactory: "0xaB45c5A4B0c941a2F231C04C3f49182e1A254052",
});
const DATA_API = "https://data-api.polymarket.com";
const PLAN_TTL_MS = 10 * 60 * 1000;

const REDEEM_ABI = [{
  type: "function",
  name: "redeemPositions",
  stateMutability: "nonpayable",
  inputs: [
    { name: "collateralToken", type: "address" },
    { name: "parentCollectionId", type: "bytes32" },
    { name: "conditionId", type: "bytes32" },
    { name: "indexSets", type: "uint256[]" },
  ],
  outputs: [],
}];

const ERC20_BALANCE_ABI = [{
  type: "function",
  name: "balanceOf",
  stateMutability: "view",
  inputs: [{ name: "account", type: "address" }],
  outputs: [{ name: "balance", type: "uint256" }],
}];
const ERC1155_APPROVAL_ABI = [{
  type: "function",
  name: "isApprovedForAll",
  stateMutability: "view",
  inputs: [{ name: "account", type: "address" }, { name: "operator", type: "address" }],
  outputs: [{ name: "approved", type: "bool" }],
}];

function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

function sha256(value) {
  return crypto.createHash("sha256").update(canonical(value)).digest("hex");
}

function normalizeAddress(value, field) {
  try {
    return getAddress(value);
  } catch {
    throw new Error(`${field} must be a valid EVM address`);
  }
}

function relayerWalletRoute(walletType) {
  const normalized = String(walletType || "").toUpperCase();
  if (normalized === "SAFE") return { walletType: normalized, txType: RelayerTxType.SAFE };
  if (normalized === "PROXY") return { walletType: normalized, txType: RelayerTxType.PROXY };
  throw new Error("walletType must be SAFE or PROXY; DEPOSIT/poly1271 custody is not supported");
}

function buildPlan(positions, context, now = Date.now()) {
  const account = normalizeAddress(context.account, "account");
  if (!/^[0-9a-f]{40}$/.test(context.releaseSha)) {
    throw new Error("releaseSha must be a lowercase 40-character commit SHA");
  }
  if (!new Set(["SAFE", "PROXY"]).has(context.walletType)) {
    throw new Error("walletType must be SAFE or PROXY");
  }

  const grouped = new Map();
  for (const position of positions) {
    if (!position.redeemable || Number(position.size) <= 0) continue;
    if (normalizeAddress(position.proxyWallet, "position.proxyWallet") !== account) {
      throw new Error("Data API position wallet does not match the live account");
    }
    const conditionId = String(position.conditionId).toLowerCase();
    if (!/^0x[0-9a-f]{64}$/.test(conditionId)) {
      throw new Error("conditionId must be bytes32");
    }
    const route = position.negativeRisk ? "neg_risk" : "standard";
    const existing = grouped.get(conditionId);
    if (existing && existing.route !== route) {
      throw new Error(`negative-risk disagreement for ${conditionId}`);
    }
    const item = existing || { conditionId, route, positions: [] };
    item.positions.push({
      asset: String(position.asset),
      outcome: String(position.outcome),
      outcomeIndex: Number(position.outcomeIndex),
      size: String(position.size),
    });
    grouped.set(conditionId, item);
  }

  const items = [...grouped.values()]
    .sort((a, b) => a.conditionId.localeCompare(b.conditionId))
    .map((item) => ({
      ...item,
      target: item.route === "neg_risk" ? CONTRACTS.negRiskAdapter : CONTRACTS.standardAdapter,
      operationId: sha256({ chainId: CHAIN_ID, account, conditionId: item.conditionId, route: item.route }),
    }));
  const unsigned = {
    version: 1,
    chainId: CHAIN_ID,
    account,
    walletType: context.walletType,
    releaseSha: context.releaseSha,
    generatedAt: new Date(now).toISOString(),
    expiresAt: new Date(now + PLAN_TTL_MS).toISOString(),
    contracts: CONTRACTS,
    items,
  };
  return { ...unsigned, sha256: sha256(unsigned) };
}

function validatePlan(plan, context, expectedHash, now = Date.now()) {
  const { sha256: embeddedHash, ...unsigned } = plan;
  const actualHash = sha256(unsigned);
  if (actualHash !== embeddedHash || actualHash !== expectedHash) throw new Error("plan hash mismatch");
  if (plan.chainId !== CHAIN_ID) throw new Error("plan chain mismatch");
  if (normalizeAddress(plan.account, "plan.account") !== normalizeAddress(context.account, "account")) {
    throw new Error("plan account mismatch");
  }
  if (plan.releaseSha !== context.releaseSha) throw new Error("plan release mismatch");
  if (plan.walletType !== context.walletType) throw new Error("plan wallet type mismatch");
  if (Date.parse(plan.generatedAt) > now + 30_000) throw new Error("plan generated in the future");
  if (Date.parse(plan.expiresAt) <= now || Date.parse(plan.expiresAt) - Date.parse(plan.generatedAt) > PLAN_TTL_MS) {
    throw new Error("plan expired or exceeds maximum TTL");
  }
  if (canonical(plan.contracts) !== canonical(CONTRACTS)) throw new Error("plan contract manifest mismatch");
  const conditionIds = new Set();
  for (const item of plan.items) {
    if (!new Set(["standard", "neg_risk"]).has(item.route)) throw new Error("unsupported redeem route");
    if (!/^0x[0-9a-f]{64}$/.test(item.conditionId)) throw new Error("plan conditionId must be bytes32");
    if (conditionIds.has(item.conditionId)) throw new Error("plan contains a duplicate conditionId");
    conditionIds.add(item.conditionId);
    const target = item.route === "neg_risk" ? CONTRACTS.negRiskAdapter : CONTRACTS.standardAdapter;
    if (normalizeAddress(item.target, "item.target") !== normalizeAddress(target, "expected target")) {
      throw new Error("redeem adapter mismatch");
    }
    const operationId = sha256({
      chainId: CHAIN_ID,
      account: normalizeAddress(plan.account, "plan.account"),
      conditionId: item.conditionId,
      route: item.route,
    });
    if (item.operationId !== operationId) throw new Error("operation id mismatch");
  }
  return plan;
}

function redeemTransaction(item) {
  return {
    to: item.target,
    value: "0",
    data: encodeFunctionData({
      abi: REDEEM_ABI,
      functionName: "redeemPositions",
      args: [CONTRACTS.pusd, zeroHash, item.conditionId, [1n, 2n]],
    }),
  };
}

async function fetchPositions(account, fetchImpl = fetch) {
  const positions = [];
  const pageSize = 500;
  for (let page = 0; page < 20; page += 1) {
    const url = new URL("/positions", DATA_API);
    url.searchParams.set("user", account);
    url.searchParams.set("limit", String(pageSize));
    url.searchParams.set("offset", String(page * pageSize));
    url.searchParams.set("sizeThreshold", "0");
    const response = await fetchImpl(url, { signal: AbortSignal.timeout(20_000) });
    if (!response.ok) throw new Error(`Data API positions failed: HTTP ${response.status}`);
    const pagePositions = await response.json();
    if (!Array.isArray(pagePositions)) throw new Error("Data API positions response is not an array");
    positions.push(...pagePositions);
    if (pagePositions.length < pageSize) return positions;
  }
  throw new Error("Data API positions exceeded the 10000-position safety limit");
}

async function verifyPositionRoutes(positions) {
  for (const position of positions.filter((candidate) => candidate.redeemable && Number(candidate.size) > 0)) {
    const url = new URL("/neg-risk", "https://clob.polymarket.com");
    url.searchParams.set("token_id", String(position.asset));
    const response = await fetch(url, { signal: AbortSignal.timeout(20_000) });
    if (!response.ok) throw new Error(`CLOB neg-risk check failed: HTTP ${response.status}`);
    const payload = await response.json();
    if (Boolean(payload.neg_risk) !== Boolean(position.negativeRisk)) {
      throw new Error(`Data/CLOB negative-risk disagreement for asset ${position.asset}`);
    }
  }
  return positions;
}

function ledgerPaths(env = process.env) {
  const ledger = env.PLOY_REDEEM_LEDGER || "/opt/ploy/data/account-ops/redeem-ledger.jsonl";
  return { ledger, lock: env.PLOY_REDEEM_LOCK || `${ledger}.lock` };
}

function appendLedger(ledger, event) {
  fs.mkdirSync(path.dirname(ledger), { recursive: true, mode: 0o700 });
  const fd = fs.openSync(ledger, "a", 0o600);
  try {
    fs.writeSync(fd, `${JSON.stringify({ at: new Date().toISOString(), ...event })}\n`);
    fs.fsyncSync(fd);
  } finally {
    fs.closeSync(fd);
  }
}

function readLedger(ledger) {
  if (!fs.existsSync(ledger)) return [];
  return fs.readFileSync(ledger, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line));
}

function requireEnv(name, env = process.env) {
  const value = env[name];
  if (!value) throw new Error(`${name} is required`);
  return value;
}

function runtimeContext(env = process.env) {
  return {
    account: requireEnv("PLOY_LIVE_ACCOUNT_ID", env),
    releaseSha: requireEnv("PLOY_RELEASE_SHA", env),
    walletType: requireEnv("POLY_WALLET_TYPE", env).toUpperCase(),
  };
}

function makeRelayClient(env = process.env) {
  const privateKey = requireEnv("POLYMARKET_PRIVATE_KEY", env);
  const account = privateKeyToAccount(privateKey);
  const rpcUrl = requireEnv("POLYGON_RPC_URL", env);
  const wallet = createWalletClient({ account, chain: polygon, transport: http(rpcUrl) });
  const builderConfig = new BuilderConfig({ localBuilderCreds: {
    key: requireEnv("POLY_BUILDER_API_KEY", env),
    secret: requireEnv("POLY_BUILDER_SECRET", env),
    passphrase: requireEnv("POLY_BUILDER_PASSPHRASE", env),
  }});
  const { walletType, txType } = relayerWalletRoute(requireEnv("POLY_WALLET_TYPE", env));
  const derived = walletType === "SAFE"
    ? deriveSafe(account.address, CONTRACTS.safeFactory)
    : deriveProxyWallet(account.address, CONTRACTS.proxyFactory);
  if (normalizeAddress(derived, "derived wallet") !== normalizeAddress(requireEnv("PLOY_LIVE_ACCOUNT_ID", env), "live account")) {
    throw new Error("signer-derived wallet does not match PLOY_LIVE_ACCOUNT_ID");
  }
  return {
    client: new RelayClient(requireEnv("POLYMARKET_RELAYER_URL", env), CHAIN_ID, wallet, builderConfig, txType),
    publicClient: createPublicClient({ chain: polygon, transport: http(rpcUrl) }),
  };
}

async function executePlan(plan, expectedHash, env = process.env, dependencies = {}) {
  if (env.PLOY_ACCOUNT_OPS_WRITE_ENABLED !== "true") throw new Error("account-ops writes are disabled");
  const context = runtimeContext(env);
  validatePlan(plan, context, expectedHash);
  const { ledger, lock } = ledgerPaths(env);
  fs.mkdirSync(lock, { mode: 0o700 });
  // ponytail: one global lock; switch to per-account locks only if account-ops throughput matters.
  let submitted = false;
  try {
    const history = readLedger(ledger);
    const relay = dependencies.relay || makeRelayClient(env);
    const { client, publicClient } = relay;
    const fetchCurrentPositions = dependencies.fetchPositions || fetchPositions;
    const verifyCurrentRoutes = dependencies.verifyPositionRoutes || verifyPositionRoutes;
    for (const item of plan.items) {
      const prior = history.filter((event) => event.operationId === item.operationId).at(-1);
      if (prior && ["submitting", "submission_unknown", "submitted", "ambiguous"].includes(prior.state)) {
        throw new Error(`operation ${item.operationId} requires reconcile`);
      }
      if (prior && ["confirmed", "reconciled", "externally_redeemed"].includes(prior.state)) continue;

      const current = await verifyCurrentRoutes(await fetchCurrentPositions(context.account));
      const candidates = current.filter((position) => position.redeemable && String(position.conditionId).toLowerCase() === item.conditionId);
      if (candidates.length === 0) {
        appendLedger(ledger, { operationId: item.operationId, conditionId: item.conditionId, state: "externally_redeemed" });
        continue;
      }
      if (candidates.some((position) => Boolean(position.negativeRisk) !== (item.route === "neg_risk"))) {
        throw new Error(`route changed for ${item.conditionId}`);
      }

      const before = await publicClient.readContract({ address: CONTRACTS.pusd, abi: ERC20_BALANCE_ABI, functionName: "balanceOf", args: [context.account] });
      const approved = await publicClient.readContract({
        address: CONTRACTS.conditionalTokens,
        abi: ERC1155_APPROVAL_ABI,
        functionName: "isApprovedForAll",
        args: [context.account, item.target],
      });
      if (!approved) throw new Error(`adapter is not approved for condition ${item.conditionId}`);
      appendLedger(ledger, { operationId: item.operationId, conditionId: item.conditionId, state: "reserved", planSha256: plan.sha256, balanceBefore: before.toString() });
      submitted = true;
      appendLedger(ledger, { operationId: item.operationId, conditionId: item.conditionId, state: "submitting", metadata: `ploy redeem ${item.conditionId}` });
      let response;
      try {
        response = await client.execute([redeemTransaction(item)], `ploy redeem ${item.conditionId}`);
      } catch (error) {
        appendLedger(ledger, { operationId: item.operationId, conditionId: item.conditionId, state: "submission_unknown", error: error.message });
        throw new Error(`relayer submission outcome is unknown for operation ${item.operationId}; reconcile by operation id`);
      }
      appendLedger(ledger, { operationId: item.operationId, conditionId: item.conditionId, state: "submitted", transactionId: response.transactionID, transactionHash: response.transactionHash || null });
      const result = await response.wait();
      if (!result || !["STATE_MINED", "STATE_CONFIRMED"].includes(result.state)) {
        appendLedger(ledger, { operationId: item.operationId, conditionId: item.conditionId, state: "ambiguous", transactionId: response.transactionID });
        throw new Error(`relayer result for ${item.conditionId} is ambiguous`);
      }
      const after = await publicClient.readContract({ address: CONTRACTS.pusd, abi: ERC20_BALANCE_ABI, functionName: "balanceOf", args: [context.account] });
      const remaining = (await fetchCurrentPositions(context.account)).some((position) => position.redeemable && String(position.conditionId).toLowerCase() === item.conditionId);
      appendLedger(ledger, {
        operationId: item.operationId,
        conditionId: item.conditionId,
        state: remaining ? "confirmed" : "reconciled",
        transactionId: response.transactionID,
        transactionHash: result.transactionHash || response.transactionHash || null,
        balanceBefore: before.toString(),
        balanceAfter: after.toString(),
      });
      if (remaining) throw new Error(`confirmed transaction still has a redeemable position for ${item.conditionId}`);
      submitted = false;
    }
    fs.rmdirSync(lock);
  } catch (error) {
    if (!submitted && fs.existsSync(lock)) fs.rmdirSync(lock);
    throw error;
  }
}

async function reconcileOperation(operationId, env = process.env, dependencies = {}) {
  if (!operationId) throw new Error("operation id is required");
  const context = runtimeContext(env);
  const { ledger } = ledgerPaths(env);
  const history = readLedger(ledger);
  const events = history.filter((event) => event.operationId === operationId);
  const latest = events.at(-1);
  if (!latest || !["submitting", "submission_unknown"].includes(latest.state)) {
    throw new Error(`operation ${operationId} is not awaiting relayer discovery`);
  }
  const submitting = events.findLast((event) => event.state === "submitting");
  if (!submitting) throw new Error(`operation ${operationId} has no submission evidence`);
  const relay = dependencies.relay || makeRelayClient(env);
  const transactions = await relay.client.getTransactions();
  const earliest = Date.parse(submitting.at) - 60_000;
  const latestCreatedAt = Date.parse(submitting.at) + PLAN_TTL_MS;
  const matches = (Array.isArray(transactions) ? transactions : []).filter((candidate) => {
    const sameMetadata = candidate.metadata === submitting.metadata;
    const sameAccount = candidate.proxyAddress
      && normalizeAddress(candidate.proxyAddress, "relayer proxyAddress") === normalizeAddress(context.account, "account");
    const createdAt = Date.parse(candidate.createdAt);
    return sameMetadata && sameAccount && createdAt >= earliest && createdAt <= latestCreatedAt;
  });
  if (matches.length !== 1) {
    throw new Error(`operation ${operationId} matched ${matches.length} relayer transactions; lock retained`);
  }
  const transaction = matches[0];
  appendLedger(ledger, {
    operationId,
    conditionId: submitting.conditionId,
    state: "submitted",
    transactionId: transaction.transactionID,
    transactionHash: transaction.transactionHash || null,
    discoveredByOperation: true,
  });
  return reconcileTransaction(transaction.transactionID, env, { ...dependencies, relay });
}

async function reconcileTransaction(transactionId, env = process.env, dependencies = {}) {
  if (!transactionId) throw new Error("transaction id is required");
  const context = runtimeContext(env);
  const { ledger, lock } = ledgerPaths(env);
  const history = readLedger(ledger);
  const submittedEvent = history.findLast((event) =>
    event.transactionId === transactionId && ["submitted", "ambiguous", "confirmed"].includes(event.state));
  if (!submittedEvent) throw new Error(`transaction ${transactionId} is not an unresolved ledger operation`);
  const operationId = submittedEvent.operationId;
  const latest = history.filter((event) => event.operationId === operationId).at(-1);
  if (latest && ["reconciled", "externally_redeemed", "failed"].includes(latest.state)) {
    return { operationId, state: latest.state, transactionId };
  }

  const relay = dependencies.relay || makeRelayClient(env);
  const transactions = await relay.client.getTransaction(transactionId);
  const transaction = Array.isArray(transactions)
    ? transactions.find((candidate) => candidate.transactionID === transactionId) || transactions.at(-1)
    : undefined;
  if (!transaction) throw new Error(`relayer has no record for transaction ${transactionId}`);

  if (["STATE_FAILED", "STATE_INVALID"].includes(transaction.state)) {
    appendLedger(ledger, {
      operationId,
      conditionId: submittedEvent.conditionId,
      state: "failed",
      transactionId,
      transactionHash: transaction.transactionHash || null,
      relayerState: transaction.state,
    });
    if (fs.existsSync(lock)) fs.rmdirSync(lock);
    return { operationId, state: "failed", transactionId };
  }
  if (!["STATE_MINED", "STATE_CONFIRMED"].includes(transaction.state)) {
    appendLedger(ledger, {
      operationId,
      conditionId: submittedEvent.conditionId,
      state: "ambiguous",
      transactionId,
      transactionHash: transaction.transactionHash || null,
      relayerState: transaction.state,
    });
    throw new Error(`transaction ${transactionId} is still ${transaction.state}`);
  }

  const fetchCurrentPositions = dependencies.fetchPositions || fetchPositions;
  const verifyCurrentRoutes = dependencies.verifyPositionRoutes || verifyPositionRoutes;
  const current = await verifyCurrentRoutes(await fetchCurrentPositions(context.account));
  const remaining = current.some((position) =>
    position.redeemable && String(position.conditionId).toLowerCase() === submittedEvent.conditionId);
  const balanceAfter = await relay.publicClient.readContract({
    address: CONTRACTS.pusd,
    abi: ERC20_BALANCE_ABI,
    functionName: "balanceOf",
    args: [context.account],
  });
  const state = remaining ? "confirmed" : "reconciled";
  appendLedger(ledger, {
    operationId,
    conditionId: submittedEvent.conditionId,
    state,
    transactionId,
    transactionHash: transaction.transactionHash || null,
    relayerState: transaction.state,
    balanceAfter: balanceAfter.toString(),
  });
  if (remaining) throw new Error(`confirmed transaction still has a redeemable position for ${submittedEvent.conditionId}`);
  if (fs.existsSync(lock)) fs.rmdirSync(lock);
  return { operationId, state, transactionId };
}

module.exports = {
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
  runtimeContext,
  sha256,
  validatePlan,
  verifyPositionRoutes,
};
