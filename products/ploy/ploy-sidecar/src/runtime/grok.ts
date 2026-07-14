import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";

export type GrokBuilderContext = {
  provider: "xai";
  model: string;
  summary: string;
};

export type GrokStrategyCompletion = {
  provider: "xai";
  model: string;
  completion: {
    status: "success" | "partial" | "blocked";
    summary: string;
    decision?: "continue" | "pass" | "trade" | "monitor" | "blocked";
    grok_decision?: "trade" | "pass" | "not_queried";
    evidence?: string[];
    blockers?: string[];
    next_action?: string;
  };
};

type FetchLike = typeof fetch;

async function queryXaiText(params: {
  messages: Array<{ role: "system" | "user"; content: string }>;
  temperature?: number;
  fetchImpl?: FetchLike;
}): Promise<{ model: string; text: string }> {
  const apiKey = process.env.XAI_API_KEY?.trim() || process.env.GROK_API_KEY?.trim();
  if (!apiKey) throw new Error("xAI/Grok API key is not configured");

  const model = process.env.XAI_MODEL || "grok-4.5";
  const endpoint = process.env.XAI_CHAT_COMPLETIONS_URL || "https://api.x.ai/v1/chat/completions";
  const fetchImpl = params.fetchImpl ?? fetch;
  const response = await fetchImpl(endpoint, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${apiKey}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      model,
      temperature: params.temperature ?? 0.2,
      messages: params.messages,
    }),
  });
  if (!response.ok) {
    throw new Error(`xAI Grok API failed: ${response.status} ${await response.text()}`);
  }

  const body = (await response.json()) as {
    choices?: Array<{ message?: { content?: unknown } }>;
  };
  const summary = body.choices?.[0]?.message?.content;
  return {
    model,
    text: typeof summary === "string" && summary.trim() ? summary : "Grok returned no text",
  };
}

export async function queryGrokBuilderContext(params: {
  objective: string;
  runPacket: string;
  runContract: string;
  fetchImpl?: FetchLike;
}): Promise<GrokBuilderContext | null> {
  const apiKey = process.env.XAI_API_KEY?.trim() || process.env.GROK_API_KEY?.trim();
  if (!apiKey) return null;

  const result = await queryXaiText({
    fetchImpl: params.fetchImpl,
    messages: [
      {
        role: "system",
        content:
          "You are Grok Builder evidence synthesis for a trading harness. Return compact evidence only. Do not recommend live orders.",
      },
      {
        role: "user",
        content: [
          `Objective:\n${params.objective}`,
          `Run packet:\n${params.runPacket}`,
          `Run contract:\n${params.runContract}`,
          "Return: grok_decision candidate (trade/pass/not_queried), evidence gaps, and confidence.",
        ].join("\n\n"),
      },
    ],
  });
  return {
    provider: "xai",
    model: result.model,
    summary: result.text,
  };
}

export async function queryGrokStrategyCompletion(params: {
  objective: string;
  runPacket: string;
  runContract: string;
  runtimeContext: unknown;
  harnessContext: string;
  fetchImpl?: FetchLike;
}): Promise<GrokStrategyCompletion> {
  const result = await queryXaiText({
    fetchImpl: params.fetchImpl,
    messages: [
      {
        role: "system",
        content:
          "You are the xAI/Grok execution engine for a diagnostic trading harness. Return one JSON object only. Do not submit orders or request deployments.",
      },
      {
        role: "user",
        content: [
          `Objective:\n${params.objective}`,
          `Run packet:\n${params.runPacket}`,
          `Run contract:\n${params.runContract}`,
          `Runtime context:\n${JSON.stringify(params.runtimeContext).slice(0, 6000)}`,
          `Harness context:\n${params.harnessContext.slice(0, 4000)}`,
          'Return JSON with keys: status ("success"|"partial"|"blocked"), summary, decision ("continue"|"pass"|"trade"|"monitor"|"blocked"), evidence array, blockers array, next_action. Include grok_decision only for Grok Builder contracts.',
        ].join("\n\n"),
      },
    ],
  });
  const completion = parseCompletionJson(result.text);
  return {
    provider: "xai",
    model: result.model,
    completion,
  };
}

function parseCompletionJson(text: string): GrokStrategyCompletion["completion"] {
  const parsed = parseJsonObject(text);
  const status = parsed.status;
  const completionStatus = status === "partial" || status === "blocked" ? status : "success";
  return {
    status: completionStatus,
    summary: typeof parsed.summary === "string" && parsed.summary.trim() ? parsed.summary : text,
    decision: parseDecision(parsed.decision),
    grok_decision: parseGrokDecision(parsed.grok_decision),
    evidence: parseStringArray(parsed.evidence),
    blockers: parseStringArray(parsed.blockers),
    next_action: typeof parsed.next_action === "string" ? parsed.next_action : undefined,
  };
}

function parseJsonObject(text: string): Record<string, unknown> {
  try {
    const parsed = JSON.parse(text);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) return parsed as Record<string, unknown>;
  } catch {
    const match = text.match(/\{[\s\S]*\}/);
    if (match) {
      try {
        const parsed = JSON.parse(match[0]);
        if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
          return parsed as Record<string, unknown>;
        }
      } catch {
        // fall through to text summary
      }
    }
  }
  return { status: "success", summary: text };
}

function parseDecision(value: unknown): GrokStrategyCompletion["completion"]["decision"] {
  return value === "continue" ||
    value === "pass" ||
    value === "trade" ||
    value === "monitor" ||
    value === "blocked"
    ? value
    : undefined;
}

function parseGrokDecision(value: unknown): GrokStrategyCompletion["completion"]["grok_decision"] {
  return value === "trade" || value === "pass" || value === "not_queried" ? value : undefined;
}

function parseStringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

async function selfTest() {
  process.env.XAI_API_KEY = "test-key";
  process.env.XAI_MODEL = "grok-test";
  const result = await queryGrokBuilderContext({
    objective: "test",
    runPacket: "packet",
    runContract: "contract",
    fetchImpl: (async (_url, init) => {
      assert.match(String(init?.body), /grok_decision candidate/);
      return new Response(
        JSON.stringify({
          choices: [{ message: { content: "grok_decision candidate: pass" } }],
        }),
        { status: 200 }
      );
    }) as FetchLike,
  });
  assert.equal(result?.model, "grok-test");
  assert.match(result?.summary ?? "", /pass/);

  const completion = await queryGrokStrategyCompletion({
    objective: "test",
    runPacket: "packet",
    runContract: "completion_signal = \"required\"",
    runtimeContext: { ok: true },
    harnessContext: "",
    fetchImpl: (async () =>
      new Response(
        JSON.stringify({
          choices: [
            {
              message: {
                content:
                  '{"status":"success","summary":"diagnostic complete","decision":"continue","evidence":["ok"],"blockers":[]}',
              },
            },
          ],
        }),
        { status: 200 }
      )) as FetchLike,
  });
  assert.equal(completion.completion.status, "success");
  assert.equal(completion.completion.summary, "diagnostic complete");
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await selfTest();
}
