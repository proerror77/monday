"use strict";

const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const { JsonRpcProvider, Wallet, getAddress, parseUnits } = require("ethers");
const { ChainId, OrderBuilder, Side } = require("@predictdotfun/sdk");

const PLAN_TTL_MS = 10 * 60 * 1000;
const MONDAY_WRITE_DISABLED = "is disabled inside Monday; route execution and reconciliation through rust_hft";

function rejectMondayWrite(operation) {
  throw new Error(`${operation} ${MONDAY_WRITE_DISABLED}`);
}
const OFFICIAL_NETWORKS = Object.freeze({
  56: "https://api.predict.fun",
  97: "https://api-testnet.predict.fun",
});

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

function normalizeContext(context) {
  const chainId = Number(context.chainId);
  if (!OFFICIAL_NETWORKS[chainId]) throw new Error("chainId must be 56 or 97");
  if (!new Set(["EOA", "PREDICT_ACCOUNT"]).has(context.accountType)) {
    throw new Error("accountType must be EOA or PREDICT_ACCOUNT");
  }
  if (!/^[0-9a-f]{40}$/.test(context.releaseSha)) {
    throw new Error("releaseSha must be a lowercase 40-character commit SHA");
  }
  return {
    account: normalizeAddress(context.account, "account"),
    accountType: context.accountType,
    chainId,
    releaseSha: context.releaseSha,
  };
}

function positiveDecimal(value, field) {
  const text = String(value);
  if (!/^(?:0|[1-9]\d*)(?:\.\d+)?$/.test(text) || !/[1-9]/.test(text)) {
    throw new Error(`${field} must be a positive decimal`);
  }
  return text;
}

function price(value, precision) {
  const text = positiveDecimal(value, "limitPrice");
  if (!Number.isInteger(precision) || precision < 0 || precision > 18) {
    throw new Error("market decimal precision must be an integer from 0 to 18");
  }
  if (!text.startsWith("0.")) throw new Error("limitPrice must be below 1");
  if ((text.split(".")[1] || "").length > precision) {
    throw new Error(`limitPrice exceeds market decimal precision ${precision}`);
  }
  return text;
}

function planEnvelope(kind, payload, context, now) {
  const normalized = normalizeContext(context);
  const unsigned = {
    version: 1,
    kind,
    ...normalized,
    generatedAt: new Date(now).toISOString(),
    expiresAt: new Date(now + PLAN_TTL_MS).toISOString(),
    ...payload,
  };
  return { ...unsigned, sha256: sha256(unsigned) };
}

function buildOrderPlan(market, input, context, now = Date.now()) {
  const marketId = Number(market.id);
  if (!Number.isSafeInteger(marketId) || marketId <= 0 || Number(input.marketId) !== marketId) {
    throw new Error("marketId mismatch or invalid");
  }
  const outcome = (market.outcomes || []).find((candidate) => String(candidate.onChainId) === String(input.tokenId));
  if (!outcome) throw new Error("tokenId is not an outcome of the market");
  const side = String(input.side).toUpperCase();
  if (!new Set(["BUY", "SELL"]).has(side)) throw new Error("side must be BUY or SELL");
  const marketRoute = {
    id: marketId,
    conditionId: String(market.conditionId).toLowerCase(),
    decimalPrecision: Number(market.decimalPrecision),
    feeRateBps: Number(market.feeRateBps),
    isNegRisk: Boolean(market.isNegRisk),
    isYieldBearing: Boolean(market.isYieldBearing),
    tradingStatus: String(market.tradingStatus),
    status: String(market.status),
    isVisible: market.isVisible === true,
    isResolved: market.resolution != null,
  };
  if (!/^0x[0-9a-f]{64}$/.test(marketRoute.conditionId)) throw new Error("conditionId must be bytes32");
  if (!Number.isInteger(marketRoute.feeRateBps) || marketRoute.feeRateBps < 0) throw new Error("invalid feeRateBps");
  if (marketRoute.status !== "REGISTERED" || marketRoute.tradingStatus !== "OPEN"
    || !marketRoute.isVisible || marketRoute.isResolved) {
    throw new Error("market must be registered, visible, open, and unresolved");
  }
  if (![1, 2].includes(Number(outcome.indexSet))) throw new Error("outcome indexSet must be 1 or 2");
  if (outcome.status != null) throw new Error("selected outcome must be unresolved");
  const quantity = positiveDecimal(input.quantity, "quantity");
  if ((quantity.split(".")[1] || "").length > 18) throw new Error("quantity exceeds 18 decimals");
  const limitPrice = price(input.limitPrice, marketRoute.decimalPrecision);
  const sideValue = side === "BUY" ? Side.BUY : Side.SELL;
  const builder = OrderBuilder.make(context.chainId === 56 ? ChainId.BnbMainnet : ChainId.BnbTestnet);
  const requestedPriceWei = parseUnits(limitPrice, 18);
  const requestedQuantityWei = parseUnits(quantity, 18);
  const amounts = builder.getLimitOrderAmounts({
    side: sideValue,
    pricePerShareWei: requestedPriceWei,
    quantityWei: requestedQuantityWei,
  });
  if (amounts.pricePerShare !== requestedPriceWei || amounts.amount !== requestedQuantityWei) {
    throw new Error("price or quantity would be truncated by the Predict SDK");
  }
  return planEnvelope("predict_fun_limit_order", {
    market: marketRoute,
    order: {
      tokenId: String(outcome.onChainId),
      outcomeIndexSet: Number(outcome.indexSet),
      outcomeStatus: String(outcome.status),
      side,
      quantity,
      limitPrice,
      feeRateBps: marketRoute.feeRateBps,
      pricePerShareWei: amounts.pricePerShare.toString(),
      makerAmount: amounts.makerAmount.toString(),
      takerAmount: amounts.takerAmount.toString(),
    },
  }, context, now);
}

function buildRedeemPlan(positions, context, now = Date.now()) {
  const items = positions.flatMap((position) => {
    const market = position.market || {};
    const outcome = position.outcome || {};
    const resolution = market.resolution;
    if (!resolution || String(resolution.status).toUpperCase() !== "WON") return [];
    if (Number(outcome.indexSet) !== Number(resolution.indexSet)) return [];
    const amount = positiveDecimal(position.amount, "position.amount");
    const conditionId = String(market.conditionId).toLowerCase();
    if (!/^0x[0-9a-f]{64}$/.test(conditionId)) throw new Error("conditionId must be bytes32");
    const marketId = Number(market.id);
    const indexSet = Number(outcome.indexSet);
    if (!Number.isSafeInteger(marketId) || marketId <= 0) throw new Error("marketId must be a positive safe integer");
    if (![1, 2].includes(indexSet)) throw new Error("position indexSet must be 1 or 2");
    return [{
      marketId,
      conditionId,
      indexSet,
      tokenId: String(outcome.onChainId),
      amount,
      isNegRisk: Boolean(market.isNegRisk),
      isYieldBearing: Boolean(market.isYieldBearing),
    }];
  }).sort((a, b) => a.conditionId.localeCompare(b.conditionId) || a.indexSet - b.indexSet);
  return planEnvelope("predict_fun_redeem", { items }, context, now);
}

function validatePlan(plan, context, expectedHash, now = Date.now(), allowExpired = false) {
  const { sha256: embeddedHash, ...unsigned } = plan;
  const actualHash = sha256(unsigned);
  if (actualHash !== embeddedHash || actualHash !== expectedHash) throw new Error("plan hash mismatch");
  const normalized = normalizeContext(context);
  if (plan.account !== normalized.account) throw new Error("plan account mismatch");
  if (plan.accountType !== normalized.accountType) throw new Error("plan account type mismatch");
  if (plan.chainId !== normalized.chainId) throw new Error("plan chain mismatch");
  if (plan.releaseSha !== normalized.releaseSha) throw new Error("plan release mismatch");
  const generatedAt = Date.parse(plan.generatedAt);
  const expiresAt = Date.parse(plan.expiresAt);
  if (!Number.isFinite(generatedAt) || !Number.isFinite(expiresAt)) throw new Error("plan timestamps are invalid");
  if (generatedAt > now + 30_000) throw new Error("plan generated in the future");
  if ((!allowExpired && expiresAt <= now)
    || expiresAt <= generatedAt || expiresAt - generatedAt > PLAN_TTL_MS) {
    throw new Error("plan expired or exceeds maximum TTL");
  }
  return plan;
}

function nowFrom(dependencies) {
  return dependencies.now ? dependencies.now() : Date.now();
}

function loadWalletSecret(env = process.env) {
  const direct = env.PREDICT_FUN_PRIVATE_KEY;
  const file = env.PREDICT_FUN_PRIVATE_KEY_FILE;
  if (Boolean(direct) === Boolean(file)) throw new Error("exactly one wallet secret source is required");
  if (direct) return direct.trim();
  const stat = fs.statSync(file);
  if (!stat.isFile() || (stat.mode & 0o077) !== 0 || (typeof process.getuid === "function" && stat.uid !== process.getuid())) {
    throw new Error("wallet secret file must be owned by the process user with mode 0600");
  }
  return fs.readFileSync(file, "utf8").trim();
}

function runtimeContext(env = process.env) {
  return normalizeContext({
    account: requireEnv("PLOY_LIVE_ACCOUNT_ID", env),
    accountType: requireEnv("PREDICT_FUN_ACCOUNT_TYPE", env).toUpperCase(),
    chainId: Number(requireEnv("PREDICT_FUN_CHAIN_ID", env)),
    releaseSha: requireEnv("PLOY_RELEASE_SHA", env),
  });
}

function requireEnv(name, env = process.env) {
  const value = env[name];
  if (!value) throw new Error(`${name} is required`);
  return value;
}

function apiHeaders(context, env, jwt) {
  const headers = { accept: "application/json" };
  if (context.chainId === 56) headers["x-api-key"] = requireEnv("PREDICT_FUN_API_KEY", env);
  if (jwt) headers.authorization = `Bearer ${jwt}`;
  return headers;
}

async function apiRequest(context, env, pathname, options = {}, fetchImpl = fetch) {
  const url = new URL(pathname, OFFICIAL_NETWORKS[context.chainId]);
  const { jwt, headers, ...requestOptions } = options;
  const response = await fetchImpl(url, {
    ...requestOptions,
    redirect: "error",
    signal: AbortSignal.timeout(20_000),
    headers: { ...apiHeaders(context, env, jwt), ...(headers || {}) },
  });
  if (!response.ok) throw new Error(`Predict API ${pathname} failed: HTTP ${response.status}`);
  const payload = await response.json();
  if (!payload || payload.success !== true) throw new Error(`Predict API ${pathname} returned success=false`);
  return payload;
}

async function fetchMarket(marketId, context, env = process.env, fetchImpl = fetch) {
  return (await apiRequest(context, env, `/v1/markets/${Number(marketId)}`, {}, fetchImpl)).data;
}

async function fetchPositions(context, env = process.env, fetchImpl = fetch) {
  const all = [];
  let after;
  for (let page = 0; page < 100; page += 1) {
    const query = new URLSearchParams({ first: "100", isResolved: "true" });
    if (after) query.set("after", after);
    const payload = await apiRequest(context, env, `/v1/positions/${context.account}?${query}`, {}, fetchImpl);
    all.push(...payload.data);
    if (!payload.cursor || payload.cursor === after) return all;
    after = payload.cursor;
  }
  throw new Error("Predict positions exceeded the 10000-position safety limit");
}

async function makeWalletSession(context, env = process.env) {
  const rpcUrl = new URL(requireEnv("PREDICT_FUN_RPC_URL", env));
  if (rpcUrl.protocol !== "https:") throw new Error("PREDICT_FUN_RPC_URL must use HTTPS");
  const provider = new JsonRpcProvider(rpcUrl.href, context.chainId, { staticNetwork: true });
  provider.pollingInterval = 300;
  const reportedChainId = Number(BigInt(await provider.send("eth_chainId", [])));
  if (reportedChainId !== context.chainId) throw new Error("Predict RPC chainId mismatch");
  const signer = new Wallet(loadWalletSecret(env), provider);
  const options = context.accountType === "PREDICT_ACCOUNT" ? { predictAccount: context.account } : undefined;
  const builder = await OrderBuilder.make(
    context.chainId === 56 ? ChainId.BnbMainnet : ChainId.BnbTestnet,
    signer,
    options,
  );
  const principal = context.accountType === "PREDICT_ACCOUNT" ? context.account : signer.address;
  if (normalizeAddress(principal, "wallet principal") !== context.account) {
    throw new Error("wallet signer does not match PLOY_LIVE_ACCOUNT_ID");
  }
  return { builder, signer };
}

async function authenticate(session, context, env = process.env, fetchImpl = fetch) {
  const challenge = await apiRequest(context, env, "/v1/auth/message", {}, fetchImpl);
  const message = challenge.data.message;
  const signature = context.accountType === "PREDICT_ACCOUNT"
    ? await session.builder.signPredictAccountMessage(message)
    : await session.signer.signMessage(message);
  const payload = await apiRequest(context, env, "/v1/auth", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ signer: context.account, signature, message }),
  }, fetchImpl);
  if (!payload.data || !payload.data.token) throw new Error("Predict auth response has no token");
  return payload.data.token;
}

function approvalScope(plan) {
  if (plan.kind === "predict_fun_limit_order") {
    return {
      operation: "TRADE",
      isNegRisk: plan.market.isNegRisk,
      isYieldBearing: plan.market.isYieldBearing,
      side: plan.order.side === "BUY" ? Side.BUY : Side.SELL,
    };
  }
  if (plan.kind === "predict_fun_redeem") {
    return plan.items.map((item) => ({
      operation: "REDEEM",
      isNegRisk: item.isNegRisk,
      isYieldBearing: item.isYieldBearing,
    }));
  }
  throw new Error(`unsupported plan kind ${plan.kind}`);
}

function dedupeSteps(steps) {
  return [...new Map(steps.map((step) => [step.id, step])).values()];
}

async function checkApprovals(plan, session) {
  const scope = approvalScope(plan);
  const scopes = Array.isArray(scope) ? scope : [scope];
  const steps = dedupeSteps(scopes.flatMap((scope) => session.builder.getApprovalSteps(scope)));
  return { steps, checks: steps.length === 0 ? [] : await session.builder.checkApprovals(steps) };
}

async function executeApprovals(plan, expectedHash, env = process.env, dependencies = {}) {
  rejectMondayWrite("Predict approval execution");
  const context = runtimeContext(env);
  validatePlan(plan, context, expectedHash, nowFrom(dependencies));
  const session = dependencies.session || await makeWalletSession(context, env);
  const scope = approvalScope(plan);
  const scopes = Array.isArray(scope) ? scope : [scope];
  const steps = dedupeSteps(scopes.flatMap((scope) => session.builder.getApprovalSteps(scope)));
  const results = [];
  for (const step of steps) {
    const [check] = await session.builder.checkApprovals([step]);
    if (check.satisfied) {
      results.push({ step, status: "skipped" });
      continue;
    }
    validatePlan(plan, context, expectedHash, nowFrom(dependencies));
    const transaction = await session.builder.setApproval(step);
    if (!transaction.success || !transaction.receipt || transaction.receipt.status !== 1) {
      throw new Error(`Predict scoped approval failed for ${step.id}`);
    }
    results.push({ step, status: "confirmed", transaction });
  }
  return { success: true, steps: results };
}

function ledgerPaths(env = process.env) {
  const ledger = env.PLOY_PREDICT_OPS_LEDGER || "/opt/ploy/data/account-ops/predict-fun-ledger.jsonl";
  return { ledger, lock: env.PLOY_PREDICT_OPS_LOCK || `${ledger}.lock` };
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

function operationId(plan) {
  return sha256({ kind: plan.kind, account: plan.account, chainId: plan.chainId, planSha256: plan.sha256 });
}

function withWriteLock(plan, env, dependencies, work) {
  const { ledger, lock } = ledgerPaths(env);
  const id = operationId(plan);
  fs.mkdirSync(path.dirname(lock), { recursive: true, mode: 0o700 });
  fs.mkdirSync(lock, { mode: 0o700 });
  fs.writeFileSync(path.join(lock, "owner.json"), `${JSON.stringify({ operationId: id, planSha256: plan.sha256, kind: plan.kind })}\n`, {
    mode: 0o600,
    flag: "wx",
  });
  const sink = dependencies.record || ((event) => appendLedger(ledger, event));
  let retain = false;
  const record = (event) => {
    sink(event);
    if (new Set(["submitting", "submission_unknown", "ambiguous"]).has(event.state)) retain = true;
    if (new Set(["submitted", "confirmed", "reconciled"]).has(event.state)) retain = false;
  };
  return Promise.resolve(work(record, id))
    .finally(() => {
      if (!retain && fs.existsSync(lock)) fs.rmSync(lock, { recursive: true });
    });
}

function assertLockOwner(lock, operationIdValue) {
  const ownerPath = path.join(lock, "owner.json");
  if (!fs.existsSync(ownerPath)) throw new Error("Predict account-op lock owner is missing");
  const owner = JSON.parse(fs.readFileSync(ownerPath, "utf8"));
  if (owner.operationId !== operationIdValue) throw new Error("Predict account-op lock belongs to another operation");
}

function assertMarketUnchanged(plan, market) {
  const current = {
    id: Number(market.id),
    conditionId: String(market.conditionId).toLowerCase(),
    decimalPrecision: Number(market.decimalPrecision),
    feeRateBps: Number(market.feeRateBps),
    isNegRisk: Boolean(market.isNegRisk),
    isYieldBearing: Boolean(market.isYieldBearing),
    tradingStatus: String(market.tradingStatus),
    status: String(market.status),
    isVisible: market.isVisible === true,
    isResolved: market.resolution != null,
  };
  if (canonical(current) !== canonical(plan.market)) throw new Error("market execution metadata changed after planning");
  const outcome = (market.outcomes || []).find((candidate) => String(candidate.onChainId) === plan.order.tokenId);
  if (!outcome) {
    throw new Error("planned token is no longer a market outcome");
  }
  if (Number(outcome.indexSet) !== plan.order.outcomeIndexSet
    || String(outcome.status) !== plan.order.outcomeStatus) {
    throw new Error("planned outcome metadata changed after planning");
  }
}

async function executeOrderPlan(plan, expectedHash, env = process.env, dependencies = {}) {
  rejectMondayWrite("Predict order execution");
  const context = runtimeContext(env);
  validatePlan(plan, context, expectedHash, nowFrom(dependencies));
  const market = dependencies.market || await fetchMarket(plan.market.id, context, env);
  assertMarketUnchanged(plan, market);
  return withWriteLock(plan, env, dependencies, async (record, id) => {
    validatePlan(plan, context, expectedHash, nowFrom(dependencies));
    const session = dependencies.session || await makeWalletSession(context, env);
    const approvals = await checkApprovals(plan, session);
    if (approvals.checks.some((check) => !check.satisfied)) throw new Error("required scoped approvals are missing");
    const side = plan.order.side === "BUY" ? Side.BUY : Side.SELL;
    const amounts = session.builder.getLimitOrderAmounts({
      side,
      pricePerShareWei: BigInt(plan.order.pricePerShareWei),
      quantityWei: parseUnits(plan.order.quantity, 18),
    });
    if (amounts.pricePerShare.toString() !== plan.order.pricePerShareWei
      || amounts.makerAmount.toString() !== plan.order.makerAmount
      || amounts.takerAmount.toString() !== plan.order.takerAmount) {
      throw new Error("SDK order amounts differ from the approved plan");
    }
    const order = session.builder.buildOrder("LIMIT", {
      side,
      tokenId: plan.order.tokenId,
      makerAmount: BigInt(plan.order.makerAmount),
      takerAmount: BigInt(plan.order.takerAmount),
      nonce: 0n,
      feeRateBps: plan.order.feeRateBps,
      expiresAt: new Date(plan.expiresAt),
    });
    const typedData = session.builder.buildTypedData(order, {
      isNegRisk: plan.market.isNegRisk,
      isYieldBearing: plan.market.isYieldBearing,
    });
    const signedOrder = await session.builder.signTypedDataOrder(typedData);
    const hash = session.builder.buildTypedDataHash(typedData);
    const jwt = dependencies.jwt || await authenticate(session, context, env);
    const submit = dependencies.submit || (async (body) => apiRequest(context, env, "/v1/orders", {
      method: "POST",
      jwt,
      headers: { "content-type": "application/json; charset=utf-8" },
      body: JSON.stringify(body),
    }));
    const latestMarket = dependencies.refreshMarket
      ? await dependencies.refreshMarket()
      : (dependencies.market || await fetchMarket(plan.market.id, context, env));
    assertMarketUnchanged(plan, latestMarket);
    validatePlan(plan, context, expectedHash, nowFrom(dependencies));
    record({ operationId: id, state: "submitting", kind: plan.kind, orderHash: hash });
    try {
      const response = await submit({ data: {
        order: { ...signedOrder, hash },
        pricePerShare: plan.order.pricePerShareWei,
        strategy: "LIMIT",
      } });
      record({ operationId: id, state: "submitted", kind: plan.kind, orderHash: hash, orderId: response.data.orderId });
      return response.data;
    } catch (error) {
      record({ operationId: id, state: "submission_unknown", kind: plan.kind, orderHash: hash, error: error.message });
      throw new Error(`Predict order submission outcome is unknown for ${id}; reconcile before retry`);
    }
  });
}

function positionAmountWei(amount) {
  const text = positiveDecimal(amount, "position.amount");
  return text.includes(".") ? parseUnits(text, 18) : BigInt(text);
}

async function executeRedeemPlan(plan, expectedHash, env = process.env, dependencies = {}) {
  rejectMondayWrite("Predict redemption execution");
  const context = runtimeContext(env);
  validatePlan(plan, context, expectedHash, nowFrom(dependencies));
  const positions = dependencies.positions || await fetchPositions(context, env);
  const fresh = buildRedeemPlan(positions, context, Date.parse(plan.generatedAt));
  if (canonical(fresh.items) !== canonical(plan.items)) throw new Error("redeemable positions changed after planning");
  return withWriteLock(plan, env, dependencies, async (record, id) => {
    validatePlan(plan, context, expectedHash, nowFrom(dependencies));
    const session = dependencies.session || await makeWalletSession(context, env);
    const approvals = await checkApprovals(plan, session);
    if (approvals.checks.some((check) => !check.satisfied)) throw new Error("required scoped approvals are missing");
    const receipts = [];
    for (const item of plan.items) {
      validatePlan(plan, context, expectedHash, nowFrom(dependencies));
      record({ operationId: id, state: "submitting", kind: plan.kind, conditionId: item.conditionId, indexSet: item.indexSet });
      const options = {
        conditionId: item.conditionId,
        indexSet: item.indexSet,
        isNegRisk: item.isNegRisk,
        isYieldBearing: item.isYieldBearing,
      };
      if (item.isNegRisk) options.amount = positionAmountWei(item.amount);
      const result = await session.builder.redeemPositions(options);
      if (!result.success || !result.receipt || result.receipt.status !== 1) {
        const transactionHash = result.receipt?.hash || result.cause?.transactionHash || result.cause?.receipt?.hash;
        record({ operationId: id, state: "ambiguous", kind: plan.kind, conditionId: item.conditionId, transactionHash });
        throw new Error(`Predict redemption is failed or ambiguous for ${item.conditionId}; reconcile before retry`);
      }
      const receipt = { conditionId: item.conditionId, transactionHash: result.receipt.hash };
      receipts.push(receipt);
      record({ operationId: id, state: "confirmed", kind: plan.kind, ...receipt });
    }
    return receipts;
  });
}

async function reconcileOrder(plan, expectedHash, env = process.env, dependencies = {}) {
  rejectMondayWrite("Predict order reconciliation");
  const context = runtimeContext(env);
  validatePlan(plan, context, expectedHash, nowFrom(dependencies), true);
  const operationIdValue = operationId(plan);
  const { ledger, lock } = ledgerPaths(env);
  assertLockOwner(lock, operationIdValue);
  const history = dependencies.history || readLedger(ledger);
  const latest = history.filter((event) => event.operationId === operationIdValue).at(-1);
  if (!latest || latest.kind !== "predict_fun_limit_order"
    || !new Set(["submitting", "submission_unknown"]).has(latest.state)) {
    throw new Error("operation is not an unknown Predict order submission");
  }
  const session = dependencies.session || await makeWalletSession(context, env);
  const jwt = dependencies.jwt || await authenticate(session, context, env);
  const lookup = dependencies.lookup || (() => apiRequest(context, env, `/v1/orders/${latest.orderHash}`, { jwt }));
  const payload = await lookup();
  appendLedger(ledger, {
    operationId: operationIdValue,
    state: "submitted",
    kind: latest.kind,
    orderHash: latest.orderHash,
    orderId: payload.data.id,
    reconciled: true,
  });
  fs.rmSync(lock, { recursive: true });
  return payload.data;
}

async function reconcileRedeem(plan, expectedHash, env = process.env, dependencies = {}) {
  rejectMondayWrite("Predict redemption reconciliation");
  const context = runtimeContext(env);
  validatePlan(plan, context, expectedHash, nowFrom(dependencies), true);
  const operationIdValue = operationId(plan);
  const { ledger, lock } = ledgerPaths(env);
  assertLockOwner(lock, operationIdValue);
  const positions = dependencies.positions || await fetchPositions(context, env);
  const current = buildRedeemPlan(positions, context, Date.parse(plan.generatedAt));
  const remaining = new Set(current.items.map((item) => `${item.conditionId}:${item.indexSet}`));
  if (plan.items.some((item) => remaining.has(`${item.conditionId}:${item.indexSet}`))) {
    throw new Error("a planned Predict redemption is still redeemable; lock retained");
  }
  const session = dependencies.balanceOf ? dependencies.session : (dependencies.session || await makeWalletSession(context, env));
  for (const item of plan.items) {
    let balance;
    if (dependencies.balanceOf) {
      balance = await dependencies.balanceOf(item);
    } else {
      const identifier = item.isYieldBearing
        ? (item.isNegRisk ? "YIELD_BEARING_NEG_RISK_CONDITIONAL_TOKENS" : "YIELD_BEARING_CONDITIONAL_TOKENS")
        : (item.isNegRisk ? "NEG_RISK_CONDITIONAL_TOKENS" : "CONDITIONAL_TOKENS");
      balance = await session.builder.contracts[identifier].contract.balanceOf(context.account, BigInt(item.tokenId));
    }
    if (BigInt(balance) !== 0n) throw new Error("planned Predict position still has an on-chain balance; lock retained");
  }
  appendLedger(ledger, { operationId: operationIdValue, state: "reconciled", kind: plan.kind });
  fs.rmSync(lock, { recursive: true });
  return { operationId: operationIdValue, state: "reconciled" };
}

module.exports = {
  OFFICIAL_NETWORKS,
  buildOrderPlan,
  buildRedeemPlan,
  checkApprovals,
  executeApprovals,
  executeOrderPlan,
  executeRedeemPlan,
  fetchMarket,
  fetchPositions,
  loadWalletSecret,
  makeWalletSession,
  normalizeContext,
  runtimeContext,
  reconcileOrder,
  reconcileRedeem,
  sha256,
  validatePlan,
};
