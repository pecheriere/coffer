# coffer plan 7 — Money redesign: overview and units

**Spec:** `docs/superpowers/specs/2026-11-07-coffer-money-redesign-design.md`. Its §10 wins over
every earlier section. Read the spec, then this overview, then the unit's own plan.

**What this is.** The spec's seven phases (§7) cut into 22 pull-request-sized units, `07a` to `07v`.
Each unit leaves `main` working and passes `scripts/gate.sh` on its own, and has its own issue,
branch, worktree and PR. Units are merged straight to `main`: there is no integration branch,
because each one is releasable by itself (where a unit would not be, it was merged with its
neighbour).

**Detailed plans exist for phases 1 to 3 only** (`07a` to `07i`), one file per unit:
`docs/superpowers/plans/2026-11-07-coffer-07<letter>-<slug>.md`. Phases 4 to 7 (`07j` to `07v`) get
their detailed plans just in time, written after phase 3 has merged, against the code as it is then;
what is below is their scope, not their plan.

## Rules for every unit

- Read `AGENTS.md` first; it is the authority on layout, wording, themes, CSP and tests.
- One git worktree and branch per unit: `git -C ~/Code/coffer worktree add -b <branch> ../coffer-07<letter> origin/main`.
  Git only as `git -C <path>`: never `cd … && git`, `git -c`, `git tag` or `git stash`.
- Tests first, and watch them fail. Files stay under about 800 lines of code: split in the same change.
- While iterating: `nice -n 15 env -u CARGO_TARGET_DIR cargo nextest run -p <crate> --all-features -j 3`.
  Never set `CARGO_TARGET_DIR`. Once before the PR: `nice -n 15 scripts/gate.sh`.
- After any API change: `cargo run -p coffer-server -- openapi > docs/api/openapi.json`. On a rebase,
  regenerate it; never hand-merge it.
- Every new number or action is in the JSON API and an MCP tool (a row in
  `crates/server/src/http/api/mcp/tools.rs` plus its parity row in `tests/it/mcp/parity.rs`), and pages
  call the same ledger call the API does.
- No local staging stack, Docker or e2e runs: CI runs those. A unit that changes the seed or e2e
  (`staging/`) says so in its PR.
- Public repo: no AI attribution in commits or PRs. The PR follows `.github/pull_request_template.md`,
  says "Closes #N", and says what was verified locally and what only CI checked.
- Scratch files only in a folder of the unit's own under the session scratchpad.

## Migration numbers

`main` ends at `0029_rules.sql`. Each unit that adds a migration owns its number, reserved here, so
units running in parallel can never pick the same one. A unit that turns out to need no migration
leaves its number unused (sqlx doesn't mind gaps); a unit that needs one it wasn't given takes the
next free number above `0037` and adds it to this table in its PR.

| Number | Unit | What |
|---|---|---|
| `0030` | 07b | `emergency_funds` (one target per currency); `household_settings` rebuilt without the floor columns |
| `0031` | 07e | `planned_items.tier` |
| `0032` | 07j | `planned_item_versions` |
| `0033` | 07n | `covers` |
| `0034` | 07o | `currency_thresholds` (auto-cover threshold) |
| `0035` | 07r | `currency_thresholds.invest_minimum_minor`, `invest_dismissals` |
| `0036` | 07t | `future_settings` (target, growth, monthly saving override, house) |
| `0037` | 07u | `future_what_ifs` |

Since the dependency graph below orders every pair of units that own migrations, they also merge in
number order.

## Units

Issue numbers are `07a` = #140 up to `07v` = #161, in letter order.

### Phase 1 — Navigation and Home shell

**07a — Navigation: Home, Money, Activity and More** · #140 · plan `07a-navigation-home-shell`
- Goal: the new navigation, Home as the landing page, the More page.
- Scope: `Nav` becomes the tabs (`Home`, `Money`, `Activity`, `More`; `Future` arrives with 07v); one
  `nav` element, a top bar on wide screens and a fixed bottom tab bar on phones, by CSS alone. `/` is
  Home (net worth with its change this month and a 12-month sparkline; transactions to check, with a
  count). `/more` lists the rest. Sign-in, the brand and error pages go to `/`. A review-queue count
  in the API (`GET /api/v1/transactions/review-count`) and MCP (`count_review_queue`).
- Touches: `server/src/http/pages/{layout.rs, mod.rs, home.rs (new), home/sparkline.rs (new), more.rs (new)}`,
  the `current:` line of every page, `assets/css/base.css`, `ledger/review_count.rs` (new),
  `store/src/transactions/count.rs` (new, a child of `transactions.rs` to reuse `push_filter`), `http/api/transactions.rs`,
  `mcp/tools.rs`, `docs/api/openapi.json`, `staging/e2e/ui/*.spec.mjs` (landing URL).
- Depends on: nothing. Parallel with 07b and 07g.
- Risk: every page's frame changes, so it conflicts textually with any page work in flight; merge it
  early. The bottom bar must not cover content or the skip link (padding at the page's foot).

**07b — Emergency fund per currency** · #141 · plan `07b-emergency-fund-per-currency` · migration `0030`
- Goal: §10.4's data model: a target per currency, fixed or N months, replacing the single floor.
- Scope: domain `EmergencyFundTarget` and `EmergencyFunds` replace `SafetyFloor`; the store's
  `emergency_funds` table and the floor's carry-over; ledger, API (`PUT`/`DELETE
  /api/v1/household/emergency-funds/{currency}`, `emergency_funds` in the settings), MCP
  (`set_emergency_fund`); the forecast reads one target per currency. The forecast's `FloorNotSet`
  blocker stays (07h retires it), now raised for a currency in use with no target.
- Touches: `domain/src/plan/{settings.rs, emergency_fund.rs (new)}`, `domain/src/forecast*`,
  `store/src/household.rs`, `store/src/emergency_funds.rs` (new), `store/src/forecast.rs`,
  `ledger/{settings.rs, emergency_funds.rs (new), forecast.rs}`, `http/api/{household.rs, forecast.rs}`,
  `mcp/tools.rs`, forecast page words, tests across `server/tests/it/forecast` and
  `household_settings.rs`, `staging/seed/household.mjs`, `docs/agent-*.md`, `docs/api/openapi.json`.
- Depends on: nothing. Parallel with 07a and 07g (07g touches `forecast/reduction.rs` and
  `bill_matching.rs` only; 07b touches `forecast.rs`, `cautions.rs`, `types.rs`).
- Risk: a wide, mechanical change to forecast tests. The months variant keeps today's meaning (all
  recurring outflows) until 07e narrows it.

**07c — Household page: emergency fund and forecast settings** · #142 · plan `07c-household-settings-page`
- Goal: nothing needs the API to set up the forecast.
- Scope: on `/admin/household`, one emergency fund form per currency in use (fixed amount or months,
  remove), and the forecast settings form (planned income, horizon, stale days). The forecast's
  blocker sentence links there.
- Touches: `pages/household.rs`, `pages/household/{emergency_fund.rs, forecast_settings.rs}` (new),
  `pages/forecast/words.rs`, `tests/it/pages/household*.rs`.
- Depends on: 07a (frame), 07b. Parallel with 07d and 07e.

**07d — Activity: what's waiting, and confirming rules' guesses** · #143 · plan `07d-activity-confirm-guesses`
- Goal: §5's Activity: the count of what waits and one action to confirm what rules filed on the page.
- Scope: the list page heads with the count (07a's ledger call); a form posts every unreviewed
  transaction on the page that only a rule filed to the existing `/transactions/batch` as reviewed.
- Touches: `pages/transactions/{list.rs, edit.rs}`, `tests/it/transactions_page/*`.
- Depends on: 07a. Parallel with 07c and 07e. No API change: the batch operation already exists.

### Phase 2 — Tiers

**07e — Tiers on planned items** · #144 · plan `07e-tiers` · migration `0031`
- Goal: every planned outflow has a tier (§4.1).
- Scope: domain `PlanTier` (`must_pay`, `essentials`, `flexible`, `saving_toward`) and its rules; the
  column with defaults; store, ledger, API, MCP; what-if items take one; the forecast's items carry it;
  an emergency fund of N months counts must pay and essentials only (§4.4).
- Touches: `domain/src/plan.rs`, `domain/src/plan/tier.rs` (new), `domain/src/forecast/{types.rs, forecast.rs}`,
  `store/src/planned_items.rs`, `store/src/forecast.rs`, `ledger/{plan_items.rs, forecast.rs}`,
  `http/api/plan_items.rs`, `tests/it/plan_items/*`, `docs/api/openapi.json`, `docs/agent-forecast.md`.
- Depends on: 07b. Parallel with 07c, 07d, 07g.
- Risk: SQLite can't add the full cross-column CHECK to a column on a table with rows (tried); the
  plan's partial CHECK plus the store's read-side refusal is the decision.

**07f — Tiers on the plan page, with tier colours** · #145 · plan `07f-tiers-plan-page`
- Goal: see and set tiers today, before Money exists.
- Scope: the plan page lists items grouped by tier; the form chooses a tier where one applies; five
  tier colour tokens in both themes (`tier-must-pay`, `tier-essentials`, `tier-flexible`,
  `tier-saving`, `tier-emergency`), in the theme contract and AA-checked as graphics.
- Touches: `pages/plan.rs`, `pages/plan/{form.rs, words.rs}`, `assets/themes/{mochi,fjord}.css`,
  `assets/css/base.css`, `themes/contract.rs`, `tests/it/plan_page/*`.
- Depends on: 07e. Parallel with 07g, 07h.

### Phase 3 — Free money and the emergency fund

**07g — A bill paid early needs nothing in the forecast** · #146 · plan `07g-bills-paid-early`
- Goal: §10.8, so free money and the forecast agree.
- Scope: occurrences up to 7 days after `as_of` join bill matching; one a payment pays needs 0 from
  that day, today's included. Shared reduction only.
- Touches: `domain/src/forecast/{reduction.rs, bill_matching.rs, lines.rs}`, domain forecast tests,
  `tests/it/forecast/bill_payments.rs`, `docs/agent-forecast.md`.
- Depends on: nothing. Parallel with everything in phases 1 and 2 (no migration, no API shape change).
- Risk: behaviour change pinned by existing tests ("dated `as_of` counts in full even if paid"); those
  tests change deliberately, named in the plan.

**07h — Free money: calculation, API and MCP** · #147 · plan `07h-free-money`
- Goal: §4.3 and §4.4's number, per currency, computed once in the domain.
- Scope: `coffer_domain::forecast::compute_free_money`; `GET /api/v1/free-money`; MCP `get_free_money`;
  the forecast's `FloorNotSet` blocker becomes the `EmergencyFundNotSet` caution in both answers
  (§10.5), with its reason in the commit.
- Touches: `domain/src/forecast/{free_money.rs, free_money/*.rs (new), forecast.rs, lines.rs, types.rs, cautions.rs}`,
  `ledger/free_money.rs` (new), `http/api/free_money.rs` (new), `http/api/{mod.rs, openapi.rs}`,
  `mcp/tools.rs`, forecast page words, `tests/it/free_money.rs` (new), `docs/agent-forecast.md`,
  `docs/api/openapi.json`.
- Depends on: 07c (its blocker link moves onto the caution), 07e, 07g. Parallel with 07f.

**07i — Home: free money and its breakdown** · #148 · plan `07i-home-free-money`
- Goal: Home leads with the answer.
- Scope: hero number in the display currency, a stacked bar (server-drawn SVG, integer maths) with its
  legend and a text equivalent, other currencies below, cautions after the answer in plain words, a
  link to the forecast. Shortfall worded as such.
- Touches: `pages/home.rs`, `pages/home/{free_money.rs, bar.rs, words.rs}` (new), `assets/css/base.css`,
  `tests/it/pages/home.rs`.
- Depends on: 07a, 07f, 07h.

### Phase 4 — Money page (plans written after phase 3 merges)

**07j — Plan history: effective-dated versions** · #149 · migration `0032`
- Goal: §10.3. Past months read the version in force then; a change applies from this month or next.
- Scope: `planned_item_versions (item_id, effective_from_month, amount, currency, category, tier,
  spending)`; migration writes one version per item from its anchor's month (goal: start month;
  one-off: its month); store reads the version for a month; the forecast, free money and plan versus
  actual resolve each occurrence's version; the API's patch takes `effective_from` (`this_month`
  default, or `next_month`) for versioned fields, and items answer their versions. No past month's
  version can be written (owner's decision, below): a request for one is 422, and neither the store
  nor the API offers a way to. "From this month" may lower a bill's amount after this month's
  occurrence was matched to a payment (owner's decision): the payment stays matched, even if the
  new amount puts it outside ±10%, and the difference is that bill's saving or overspend (§10.7).
  So the match of an occurrence already paid is not re-run against the new amount; the plan says
  how (for example, matching this month's paid occurrences against the amount they were matched
  at) and pins it with a test: a 1,000 bill paid 1,000, lowered to 800 from this month, still needs
  0, shows 200 over, and raises no unpaid caution.
- Why here: the spec's phases put it nowhere; it must precede the Money page (past months, #120) and
  covers (a cover adjusts one month of one version), and it is easier once tiers exist, so the tier
  moves into versions with the rest instead of being designed twice.
- Depends on: 07e, 07h (so the free money read is versioned in the same change). Parallel with 07i.
- Risk: the forecast's `ItemInput` gets a per-month amount; every reducer of items changes.

**07k — Buckets: this month and past months (API, MCP)** · #150
- Goal: §4.2 and §6's buckets read, built on `plan_report`. Closes #120 (plan versus actual matches
  bills with `bill_matching`, and past months use their versions) and #134 (a goal's set aside so far
  in the plan items API and here).
- Scope: `GET /api/v1/buckets?month=YYYY-MM`: per bucket planned, spent, left or over, pace (only for
  budgets repeating monthly or more often, §10.10), goals' set aside against target and date, the
  emergency fund; MCP `get_buckets`.
- Depends on: 07j.

**07l — Money page and a bucket's page** · #151
- Scope: `/money` replaces the `Money` tab's target (`/plan`), totals, one group per tier with bars and
  pace markers, the emergency fund, month switcher; `/money/buckets/{id}`: this month, last 6 months,
  pause. Bars are integer SVG with text equivalents.
- Depends on: 07f, 07k.

**07m — Change the plan from Money, one form per kind** · #152
- Scope: add and edit forms per kind (bill, budget, income, one-off, goal), "from this month" or "from
  next month"; `/plan` redirects to `/money`; the old combined form goes.
- Depends on: 07j, 07l.

### Phase 5 — Covers and overspend

**07n — Covers** · #153 · migration `0033`
- Scope: `covers (id, month, from_item, to_item, amount, currency, member, agent, created_at)`; domain
  rules by tier (§4.1, §4.6.3; never from must pay, from saving toward and the emergency fund only when
  chosen, never investments); a goal cover draws on the goal (§10.2), an emergency fund cover is a
  record; buckets and free money apply covers; `POST/GET/DELETE /api/v1/covers`; MCP tools for create
  and list (undo stays REST, as MCP deletes nothing).
- Depends on: 07k.

**07o — Needs a look and the auto-cover threshold (API, MCP)** · #154 · migration `0034`
- Scope: a derived list (§10.6): overspends covered from free money this month, overspends needing a
  source (free money can't absorb them), bills over or under within the ±10% window (§10.7), buckets
  ahead of pace past 25% of the month with their run-out day (not for budgets repeating less often
  than monthly), transactions to check. Threshold per currency (default 100 major units, planner's
  call), in `currency_thresholds`. `GET /api/v1/needs-a-look`, settings endpoint; MCP.
- Depends on: 07n.

**07p — Home's Needs a look and the cover sheet** · #155
- Scope: the Home section; a cover sheet page (works without JS) listing allowed sources with their
  effect, or "change the plan" from next month; covers with undo on a bucket's page; threshold form on
  Household.
- Depends on: 07l, 07m, 07o.

### Phase 6 — Keep or invest and the 3-month chart

**07q — The 3-month cash projection and its low point** · #156
- Scope: a day-level projection over 3 months (§4.5, §10.1: counts planned income whatever
  `include_income` says; subtracts goal accrual as it happens, §10.9), the low point and its date and
  cause; in the free money answer or its own endpoint; MCP.
- Depends on: 07h, 07j.

**07r — Keep or invest** · #157 · migration `0035`
- Scope: suggestion `free money − cushion` (§4.5), shown at or above the invest minimum per currency
  (default 500 major units), withheld with no emergency fund (§10.5); "Not now" stores the dismissed
  amount (§10.6); API, MCP, Household form for the minimum.
- Depends on: 07c, 07q, 07o (0035 alters 07o's `currency_thresholds`; found while planning 07r).

**07s — Home's cash chart and invest card; forecast page answers first** · #158
- Scope: Home's 3-month chart with the emergency fund line and the low point; the invest card with
  "Not now"; the forecast page drops its "Surplus" headline and explains the projection and the
  suggestion (§10.9).
- Depends on: 07i, 07r.

### Phase 7 — Future (secondary)

**07t — Future projection, range and target** · #159 · migration `0036`
- Scope: domain projection from the daily net worth table: monthly saving from the last 12 months'
  income minus spending, real growth (default 5%, ±1.5 points), house excluded by default
  (`illiquid` accounts and their linked loans), horizon 25 years, in today's money, as exact integer
  maths (rates in basis points); target date and range; stored settings; API, MCP.
- Depends on: 07a only. Can start any time after phase 1, in parallel with phases 2 to 6, if hands
  allow.

**07u — Future what-ifs** · #160 · migration `0037`
- Depends on: 07t.

**07v — Future page** · #161
- Scope: the `Future` tab, chart with range and target line, milestones (goals' due dates, mortgage
  end), input chips, what-ifs, the overspending note.
- Depends on: 07t, 07u, 07k (the overspending note reads this month's `over` from buckets; found while planning 07v).

## Order and parallelism

| Wave | Units, in parallel | Waits for |
|---|---|---|
| 1 | 07a, 07b, 07g | — |
| 2 | 07c, 07d, 07e | 07a (c, d), 07b (c, e) |
| 3 | 07f, 07h | 07e (f), 07c + 07e + 07g (h) |
| 4 | 07i, 07j | 07a + 07f + 07h (i), 07h (j) |
| 5 | 07k | 07j |
| 6 | 07l, 07n, 07q | 07f + 07k (l), 07k (n), 07h + 07j (q) |
| 7 | 07m, 07o | 07l (m), 07n (o) |
| 8 | 07p, 07r | 07l + 07m + 07o (p), 07c + 07o + 07q (r) |
| 9 | 07s | 07i + 07r |
| any after 07a | 07t → 07u; 07v after 07u and 07k | — |

What parallel units still share, and how to merge them: `docs/api/openapi.json` (the second to merge
regenerates it), `mcp/tools.rs` and `tests/it/mcp/parity.rs` (rows added at different places: keep both;
a unit pushing `tools.rs` past about 700 lines splits it by area, `tools/{transactions,plan,household}.rs`,
in the same change; `parity.rs` is already at 755 lines, so the first unit adding a parity row (07a or
07b, whichever merges first) splits it into `tests/it/mcp/parity/{mod.rs, …}` by area in that change,
and the other rebases onto the split), `tests/it/main.rs` (one `mod` line each). 07a touches every page's frame line,
so units starting after it rebase onto it.

At most about three units build at once on the shared Mac (`AGENTS.md`, owner's load limits).

## Planner's calls

Minor gaps settled here; each is reversible by changing this list and the unit.

1. **`/account` keeps its URL** and is labelled "You" under More: passkey ceremonies, grants and e2e
   tests post to it, and a move buys nothing.
2. **No `Future` tab until 07v**, and `Money` points at `/plan` until 07l: no tab leads to an empty page.
3. **Emergency fund lives in its own table**, one row per currency with a target; no row means none set.
   The months variant is per currency: N months of that currency's outflows (all recurring outflows in
   07b, must pay and essentials from 07e). Removing a target stays REST-only; MCP sets but deletes
   nothing.
4. **The floor's carry-over:** a fixed floor becomes its currency's target; a months floor becomes a
   months target for every currency the forecast may use (accounts in the forecast, closed ones too since
   a closed card still owing counts, active planned items of either direction, the display currency).
5. **The API breaks once, deliberately:** `floor` leaves the settings, its patch, the forecast and the
   what-if; `emergency_funds` replaces it (07b), and `CurrencyTotals.floor` becomes `emergency_fund`.
   The owner's agents read the schema through MCP; no deprecation period for a single household.
6. **Tier values and rules:** a goal is always `saving_toward` and nothing else is; a recurring or
   one-off outflow is `must_pay`, `essentials` or `flexible` (a bill may be any of the three); income
   has none. The tier changes only when sent, or when an item stops or starts being an outflow. The
   emergency fund is not a planned item, so it is not a tier value in the API: it is its own line.
7. **Tier's CHECK is partial** (values, and "null or consistent with direction and type"), because
   SQLite refuses a cross-column CHECK on `ADD COLUMN` over existing rows (tried with 3.54) and
   rebuilding `planned_items` would cascade-delete settlements. The store refuses an outflow read
   without a tier as corrupt; 07j's versions table carries the tier from then on.
8. **Free money's status reuses the forecast's**: blockers and cautions of a one-month forecast at
   today, without the forecast-only cautions; a currency whose account has no balance has no free money
   (`null`), others still answer. A completed goal sets aside nothing, as in the forecast.
9. **The review-queue count** is its own small endpoint rather than a count on every list page.
10. **Thresholds per currency** (auto-cover 100, invest minimum 500, in each currency's major units),
   since amounts are never converted between currencies.
11. **The forecast page keeps its surplus until 07s**, where its explanation of the projection and the
   suggestion exists to replace it (§10.9). Until then it is reached from More and Home's link only.
12. **Plan history is unit 07j**, between free money and the Money page (see 07j).

## Owner's decisions on plan history

Settled by the owner on 2026-11-07; 07j's plan follows them.

1. **"From this month" may lower a bill's amount after its payment was matched this month.** The
   payment stays matched; the bill's spent is its payment and the difference is a saving or an
   overspend (§10.7).
2. **Past months' plan versions cannot be edited.** Only this month's and next month's (§10.3).
