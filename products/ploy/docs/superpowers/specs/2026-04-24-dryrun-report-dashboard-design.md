# Dry-Run Report Dashboard Design

Date: 2026-04-24
Status: proposed
Owner: Codex

## Goal

Replace the current single-strategy static HTML report with a clearer operator-facing dry-run monitoring surface for one primary user: the repo owner monitoring multiple dry-run strategies on `tango-1-1`.

The new surface should optimize for:

- fast daily scanning across multiple strategies
- quick identification of which strategy needs attention
- one-click drill-down into a single strategy
- trading-result-first readability

The new surface should not optimize for:

- public marketing presentation
- parameter tuning visibility
- external stakeholder explanation
- research notebook depth

## User Decisions Locked

The following product choices are already approved:

- Information architecture: homepage overview of all strategies, then drill down into one strategy
- Detail-page priority: trading results first
- Most prominent metric: today's PnL
- Strategy parameters: hidden by default
- Audience: the user themself, not external viewers

## Problem

The current report works as a single-strategy artifact, but it is not a good daily monitoring surface once multiple dry-run strategies exist.

Current issues:

- it assumes one strategy at a time
- it reads like a report, not a monitor
- the information hierarchy is weak for "what needs my attention now?"
- the page spends too much space on low-priority detail
- the parameter table adds noise for day-to-day review

## Recommended Approach

Build a two-level dashboard:

1. `/reports/strategies`
   A multi-strategy overview page with compact cards, sorted by urgency
2. `/reports/strategy?strategy_id=<id>`
   A single-strategy detail page focused on today's outcome and recent behavior

This is the recommended approach because it matches the user's actual workflow:

- scan all strategies first
- identify the one that needs attention
- inspect only that strategy in detail

## Alternative Approaches Considered

### Option A: Two-level overview plus detail

Pros:

- best scanability for multiple strategies
- scales naturally from one strategy to many
- preserves a simple mental model
- keeps the detail page clean

Cons:

- requires one more click for detail

Recommendation:

- choose this option

### Option B: One giant page with all strategies stacked vertically

Pros:

- simple implementation
- no navigation model needed

Cons:

- degrades quickly as strategy count grows
- poor comparability
- harder to spot anomalies
- encourages visual clutter

Rejected because it does not fit a daily monitoring workflow.

### Option C: Pure aggregate portfolio page with strategy filters

Pros:

- strong top-level summary
- useful if the main concern is portfolio-level risk

Cons:

- weak strategy-level scanability
- the user explicitly wants strategy-by-strategy monitoring
- makes single-strategy comparison slower

Rejected because the first question the user wants answered is "which strategy is doing what today?"

## Information Architecture

### Page 1: Multi-Strategy Overview

Path:

- `/reports/strategies`

Primary purpose:

- answer "which strategy is up, down, idle, or unhealthy today?"

Structure:

1. Header
   - title
   - last refresh timestamp
   - optional `since` filter
2. Summary strip
   - total dry-run strategies
   - strategies green today
   - strategies red today
   - strategies with open positions
   - strategies with stale/no recent activity
3. Strategy card grid
   - each card represents one strategy or deployment
4. Optional compact watchlist row
   - "needs attention now" cards only

### Page 2: Single-Strategy Detail

Path:

- `/reports/strategy?strategy_id=<id>`

Primary purpose:

- answer "how is this specific strategy performing today and what just happened?"

Structure:

1. Header
   - strategy name
   - current status badge
   - link back to all strategies
2. Primary KPI row
   - today's PnL as the dominant metric
3. Secondary KPI row
   - cumulative PnL
   - win rate
   - max drawdown
   - trades today
4. Charts
   - cumulative PnL
   - daily PnL
   - symbol contribution
5. Tables
   - recent trades
   - open positions
   - per-symbol summary

The detail page should not show the config/parameter table by default.

## Strategy Card Design

Each strategy card should be compact and decision-oriented.

Fields:

- strategy display name
- optional deployment id
- status badge: running, idle, degraded, stopped
- today's PnL
- cumulative PnL
- trades today
- open positions count
- last trade time
- one small sparkline for recent cumulative movement

Visual rules:

- today's PnL is the largest number on the card
- cumulative PnL is secondary
- badges use restrained color, not oversized labels
- cards sort by urgency, not alphabetically

Urgency sort order:

1. degraded or error state
2. negative today's PnL
3. open positions with stale recent activity
4. active profitable strategies
5. inactive/no-data strategies

## Detail Page Design

### Above the fold

The first viewport should answer:

- is this strategy making or losing money today?
- is it alive?
- is it trading now?

So the top section should include:

- strategy title
- status badge
- today's PnL in the largest type
- trades today
- cumulative PnL
- win rate

### Middle section

The middle section should help explain the result:

- cumulative PnL chart
- daily PnL bars
- symbol contribution bars

### Lower section

The lower section should support investigation:

- recent trades table
- open positions table
- per-symbol breakdown table

### Hidden/removed content

Do not show:

- raw strategy parameters
- giant config dumps
- research-only diagnostics
- implementation details like throttle or execution knobs

## Data Model for the Dashboard

The monitoring surface should group by strategy first, not by individual fill.

The canonical grouping key should be:

- `strategy_id`

Optional secondary grouping:

- `deployment_id`

This allows one strategy to have one or more dry-run deployments later without changing the page model.

Per-strategy aggregates required:

- today realized pnl
- cumulative realized pnl
- total closed trades
- today's closed trades
- win rate
- max drawdown
- open positions count
- open exposure
- last trade time
- per-symbol pnl

## Multi-Strategy Presentation Rules

If only one strategy exists:

- `/reports/strategies` should still render the overview card layout
- it can auto-link prominently into the detail view

If multiple strategies exist:

- every strategy gets one card
- cards must remain compact and comparable
- the page should not explode into nested sections

If a strategy has zero trades:

- show it as a muted card
- do not place it ahead of active strategies

If a strategy is stale:

- show a warning badge such as `stale`
- stale means either:
  - deployment says running but no recent trades for a configurable window, or
  - no snapshot freshness from the current source of truth

## URL and Navigation

Add:

- `/reports/strategies`
- `/reports/strategy?strategy_id=<id>`

Optional later:

- `/reports/strategy?strategy_id=<id>&since=YYYY-MM-DD`

Navigation rules:

- the overview page links every card to the strategy detail page
- the detail page includes a clear back link
- no nested router complexity is needed for the first version

## Rendering Strategy

Recommended implementation:

- keep server-side HTML generation for now
- split the current monolithic `report_strategy.py` into reusable data query + render helpers
- add a new multi-strategy report generator alongside the single-strategy report
- keep `ployd` as the HTTP delivery layer

Reason:

- this is the smallest path from the current working system
- it preserves the deployed operational model on `tango-1-1`
- it avoids inventing a second rendering stack before the data model is proven

Not recommended yet:

- rewriting this immediately as a new SPA in `ploy-frontend`
- introducing Java or another separate web service

Those can come later if the dashboard needs richer interactions, but they are not required to solve the user's current monitoring problem.

## Visual Style

The page should look like an internal operator dashboard, not a blog report.

Style targets:

- dark background is acceptable and already compatible with the current report
- tighter information density
- less decorative empty spacing
- stronger hierarchy for top metrics
- smaller charts with clearer labels
- more use of compact cards and status chips

Avoid:

- giant hero headers
- oversized narrative text
- visible config blocks
- decorative gradients
- section cards inside section cards

## Error Handling

If a strategy has incomplete data:

- show a card with a warning badge
- do not fail the whole page

If one chart dataset is unavailable:

- keep the rest of the strategy page rendered
- show a compact inline empty-state block for the missing section

If report generation fails entirely:

- return an operator-readable error page
- include the failure source at a high level
- do not dump stack traces into the browser

## Testing and Verification

Minimum verification for implementation:

1. local generation of:
   - one multi-strategy overview page
   - one single-strategy detail page
2. `ployd` route tests for:
   - `GET /reports/strategies`
   - `GET /reports/strategy?strategy_id=...`
3. remote verification on `tango-1-1`:
   - route returns `200`
   - page includes expected title and current timestamp
   - at least one real strategy card appears
4. public-path verification:
   - nginx route exposes only the intended report paths
   - other paths remain closed if not explicitly allowed

## Implementation Scope Boundaries

Included:

- redesign of report information architecture
- multi-strategy overview page
- improved single-strategy detail page
- removal of parameter display from the default view
- remote generation and serving on `tango-1-1`

Not included:

- auth redesign
- full frontend rewrite
- strategy editing from the page
- research notebook views
- live trading controls on the report pages

## Recommendation

Proceed with a server-rendered two-page dashboard:

- `/reports/strategies` for overview
- `/reports/strategy?strategy_id=<id>` for detail

Keep the implementation on the current `ployd + report script + nginx` path.
This is the lowest-risk way to get a better-looking, easier-to-read, multi-strategy dry-run monitor onto `tango-1-1` quickly.
