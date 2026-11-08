# Coffer money redesign: free money, buckets and where net worth is heading

Date: 2026-11-07. Status: approved direction (owner, from the mockups below).
Mockups: a private page the owner reviewed (Fjord theme, example
numbers). The research behind it is summarised in §9.

This document changes one decision of `2026-10-30-coffer-design.md`: §3 lists "zero-based /
envelope budgeting à la YNAB" as a non-goal. That stays true for the *ritual* (assigning every
dollar each month), but coffer now has **buckets with priorities that adapt live**. Update §3 of
the original design to point here.

## 1. Intent

The owner wants to open coffer every so often, not daily, and answer two questions without
micromanaging:

1. **Near term: what can I do with my money?** How much cash is genuinely free after what is
   already spoken for, and should the surplus stay as cash or be invested?
2. **Long term: where is my net worth heading?** And, as a secondary goal for later: "my
   target is $X, when do I reach it?"

Bookkeeping happens on its own: transactions sync, rules file them, and buckets draw down
live. The owner steps in only for decisions (an overspend that can't be covered on its own, a
surplus that could be invested, transactions to check).

The owner likes simple, elegant, easy UI. Reuse coffer's existing design language and themes
(Mochi, Fjord, tokens in `crates/server/assets/themes/`, components in `base.css`). The mockups
use Fjord's tokens.

## 2. Goals, non-goals, anti-goals

**Goals**

- One honest **free money** number on a new Home page, live as transactions sync.
- **Buckets** set once as a recurring plan, each with a **tier** (priority) that decides
  where an overspend may be covered from.
- **Overspend coverage** that follows the tiers, automatic for small amounts, asked for large
  ones, always visible and reversible.
- A **keep or invest** suggestion with one rule (§4.5).
- A **3-month cash chart** with its low point.
- **Navigation** cut to Home, Money, Future, Activity, More. Home is the landing page.
- **Future**: net worth trajectory with a range and a target date (later phase).
- Everything stays agent-first: every new number and action is in the JSON API and its MCP
  tools, and pages call the same ledger service as the API (existing rule, AGENTS.md).

**Non-goals**

- Monthly assignment rituals, "Ready to Assign", auto-assign buttons.
- Coffer moving real money. It never initiates transfers.
- Investment advice, holdings, returns per security (still a non-goal of the original design).
- Monte Carlo, success percentages, tax modelling in Future.
- Leftover-rollover options per bucket (v1: see §4.2).

**Anti-goals** (failure even if it technically works)

- The owner has to pick a source for every overspend (YNAB's chore).
- Money set for a must-pay bill is used to cover restaurants.
- A number that can't be explained in one tap: every headline figure has its breakdown one
  step away.
- Jargon on the page ("accrual", "provisional", "settled") where plain words exist; a page
  that leads with warnings instead of the answer.
- Investments silently counted as spendable, or sold to cover spending without the owner
  saying so.

## 3. Words

Use these words on pages, in the API's descriptions and in MCP tool texts:

| Word | Means |
|---|---|
| **Free money** | Cash nobody has claimed yet (§4.3). |
| **Bucket** | A planned item seen as money set for a purpose. The UI says "bucket"; the API keeps `plan item` names, with `tier` added. |
| **Tier** | Must pay, Essentials, Flexible, Saving toward, Emergency fund. |
| **Cover** | Taking an overspend from another source. |
| **Emergency fund** | The old safety floor, now a bucket you can spend from (§4.4). |

## 4. Behaviour

### 4.1 Buckets are planned items with a tier

Builder's starting point, owner-approved concept: **do not build a parallel system**. Evolve
planned items (`crates/domain/src/plan*`):

| Tier | Typical planned items | Amount | Can lend to |
|---|---|---|---|
| Must pay | Bills (mortgage, insurance), budgets for utilities, property tax | fixed, or estimate | nothing, ever |
| Essentials | Budgets: groceries, fuel, kids | estimate | other Essentials |
| Flexible | Budgets: restaurants, hobbies, shopping | estimate or cap | Essentials and Flexible |
| Saving toward | Goals: trip, car, gifts | target by a date | anything, only when the owner confirms |
| Emergency fund | The household's floor (§4.4) | target | anything, only when the owner confirms |

- Every planned outflow gets a `tier`. Defaults when migrating: Bill → Must pay; Budget →
  Flexible (the owner re-tiers essentials once); Goal → Saving toward; OneOff → Must pay.
  Income items have no tier.
- A Budget bucket draws from its category's spending, as budgets already do (R10 of the
  original design). Several budgets sharing a category keep today's behaviour (the category's
  pool pays them in date order, `reduction.rs`); no new restriction.
- Uncategorized spending draws from free money directly.

### 4.2 This month's state of a bucket

For the current month, each bucket shows: planned, spent so far, left (or over), and for
estimate buckets a **pace** (share of the month elapsed versus share spent). Goals show set
aside so far against target and date (existing goal accrual), with "on track" or "$N short".

Leftover at month end: Must pay, Essentials and Flexible leftovers go back to free money; Saving
toward keeps accruing as goals already do. (My call; per-bucket rollover is a non-goal for now.)

### 4.3 Free money

For a currency, today:

```
free money = cash available
           − what this month's Must pay, Essentials and Flexible buckets still need
           − what Saving toward buckets have set aside so far
           − emergency fund target
```

- "Cash available" is what the forecast already calls it (accounts counted as cash, minus
  short-term debts). Investments never count.
- "Still need" is `max(planned − spent, 0)` per bucket, after covers (§4.6). Bills already paid
  this month (matched or settled) need 0.
- The breakdown in that order is shown under the number (the mockup's stacked bar and legend).
- Amounts are never netted across currencies (existing rule): one free money per currency
  (and one emergency fund target per currency, §10.4);
  Home leads with the display currency's and lists others below. Use the forecast's existing
  status honesty (blockers, cautions) but put them below the answer, in plain words.
- It is computed in the domain crate (pure, no I/O), next to `forecast`, from the same data
  read the forecast uses, so the API, MCP and the page agree.

### 4.4 Emergency fund

The safety floor becomes a bucket in the Emergency fund tier, with a target (fixed amount or N
months of Must pay plus Essentials: the existing `SafetyFloor` variants, `MonthsOfOutflows`
reinterpreted as those two tiers' monthly total).

Stateless by design (my call, simplest honest model):

```
before = cash available − still need − set aside
emergency fund held = clamp(before, 0, target)
free money = max(before − target, 0)       (shortfall shown when before < 0)
```

So spending from the emergency fund is just spending beyond free money, and it **refills from
free money first** automatically. Coffer asks the owner to confirm when an overspend would dip
into it (§4.6). It is editable on a page: fixes the current "set it through the API" blocker
(`forecast/words.rs:151`). With no target set, free money still shows, with a caution "No
emergency fund set" and a link to set one; the forecast's hard blocker goes away.

### 4.5 Keep or invest

One rule: suggest investing `free money − cushion`, where the cushion is what keeps the
**lowest projected cash over the next 3 months** at or above `emergency fund target + still
need + set aside` at that day. Show it only when the suggestion is at least a household-set
minimum (default $500, my call). Words: "You could invest $3,000. That still leaves $1,180
free, and the next 3 months never dip into the emergency fund, even at the low point (Dec 14,
property tax)."

No "I moved it" button that records anything (my call, differs from the mockup): coffer never
moves money, and the transfer shows up by itself once it syncs, which lowers free money. The
card offers "Not now" (hides it for this amount until free money changes by more than the
minimum).

The projection reuses the forecast engine. It needs the **low point and its date**, so the
builder decides whether to add day-level resolution to the forecast's month buckets or compute
the low point separately; either way it comes from the domain crate and is in the API.

### 4.6 Overspend and cover

When a bucket's spent exceeds its planned amount this month:

1. The excess is taken from **free money** by default. No write is needed: free money already
   drops, since the bucket's "still need" is 0 and the cash is gone. Home shows "Restaurants
   went $64 over · covered from free money · Change".
2. The owner may **re-source** it: take it from another bucket instead. That is a **cover**,
   a stored record: month, from bucket, to bucket, amount, who, when. A cover lowers the source
   bucket's planned amount for that month and raises the target's, so free money comes back.
   Covers are audited, listed on the bucket's page, and can be undone.
3. Sources allowed by tier (§4.1). Must pay is never a source. Saving toward and Emergency fund
   need the owner's explicit choice and show the effect ("moves the goal back about a week").
   Investments are never a source.
4. **When free money can't absorb it** (free money would go below 0, so the emergency fund or a
   shortfall is next), Home asks: "Restaurants went $640 over. Free money covers $180; take the
   rest from: Hobbies, Clothes, Japan trip, Emergency fund". Nothing is moved until the owner
   picks; meanwhile the number honestly shows the emergency fund dipping.
5. **Auto-cover threshold** (household setting, default $100, my call): an overspend at or below
   it is covered from free money silently and listed on Home after the fact; above it, Home lists
   it as needing a look even when free money absorbs it.
6. From the cover sheet the owner can instead **change the plan** ("Raise Restaurants to $350 a
   month"), which edits the bucket from next month.

Heads-up (my call, from the mockup): an estimate bucket spending ahead of pace gets "Groceries
runs out around Nov 24 at this pace" on Home, computed from spent-to-date and days elapsed.
Shown only past 25% of the month, to avoid noise on the 2nd.

### 4.7 Future (later phase, secondary)

- One chart: net worth history (existing daily values) plus a projection to a horizon (default
  25 years), in today's money, with a range (pessimistic, likely, optimistic growth). A
  horizontal line at the owner's **target**, and the headline "You reach $2M around 2041,
  somewhere between 2039 and 2045".
- Inputs with defaults from real data, shown as editable chips: monthly saving (the last 12
  months' income minus spending, from transactions), target, real growth rate (default 5%, range
  ±1.5 points), whether the house counts (default no: accounts in the "hard to sell" liquidity
  class and their linked loans are left out).
- What-ifs: a one-off purchase, a change in monthly saving from a year, a year without income.
  Each draws a dotted line and states the change in months. Stored per household so agents can
  read and add them.
- The cost of this month's overspending in future money ("$310 more than planned, about $610 by
  2040 if left invested"), shown once as a calm note, never a nag.
- Milestones (goals with dates, the mortgage end) as markers.
- Remembering past projections to compare with actuals is out of scope for now.

## 5. Pages

Navigation: a bottom tab bar on phones, the same five links in the top bar on wide screens:
**Home, Money, Future, Activity, More**. `/` and sign-in land on Home. The brand links Home.

- **Home**: free money with its breakdown; the keep-or-invest card; "Needs a look" (covered
  overspends with Change, overspends that need a source, transactions to check with a count,
  buckets ahead of pace, the forecast's cautions in plain words); net worth with change this
  month and a 12-month sparkline, linking to its history; the 3-month cash chart with the
  emergency fund line and the low point. Ends with a link to the full cash forecast.
- **Money**: this month (switchable to past months: plan vs actual), totals planned, spent and
  free money, then one group per tier, each bucket with its bar, pace marker and left/over, then
  the emergency fund. Tapping a bucket opens its page: this month, covers, the last 6 months,
  change or pause it. "Change the plan" adds and edits buckets; this replaces today's Plan page
  (its add form split by kind: one form per kind, not one form with every fieldset).
- **Future**: §4.7.
- **Activity**: today's transactions list and review queue, with the count of what's waiting
  and one action to confirm all of rules' guesses on the page.
- **More**: Accounts, Net worth history (today's net worth page), Cash forecast detail (today's
  forecast page, reworded, answer first), Categories, Rules, Connections, Household (members,
  currency, theme, emergency fund, auto-cover threshold, invest minimum), You (today's
  `/account`: passkeys, tokens, appearance).

The mockups are the reference for layout and tone. Constraints the real pages must keep
(AGENTS.md, and the audit of today's pages):

- CSP `default-src 'self'; script-src 'self'; style-src 'self'`: no inline scripts or styles and
  no `style=` attributes. Bars and charts are server-drawn SVG (integer maths, classes only, like
  `net_worth/chart.rs`), each with a table or text equivalent. No JS framework; flows work as
  plain forms with POST-redirect; the cover sheet is a page or `<dialog>` that works without JS.
- Theme rules: colours and fonts only from tokens; tier colours become new tokens in both
  themes, AA-checked by the existing theme tests.
- Read-only roles see no forms. Wording tests (`word_checks.rs`) cover new kinds.
- Files stay under about 800 lines; split by responsibility.

## 6. API and MCP

New or changed, each with OpenAPI entries, MCP tools and authorization like their neighbours:

- Plan items gain `tier`. Validation per §4.1.
- `GET` free money (per currency: the breakdown of §4.3, emergency fund held, status, cautions).
- `GET` this month's buckets (planned, spent, left, pace, covers), or any past month (plan vs
  actual, building on `plan_report`; resolve #120's disagreement between plan vs actual and the
  forecast about what paid a bill in the same change, since Money depends on it).
- Covers: create, list, undo.
- Household settings: emergency fund target, auto-cover threshold, invest minimum.
- Keep-or-invest suggestion and the 3-month low point (may be part of the free money response).
- Later: Future projection, target, what-ifs.

## 7. Phasing (for the plan; order is my call)

1. **Navigation and Home shell**: tab bar, Home as landing, More page, `/account` moved to You;
   emergency fund and household settings editable on pages. Home shows net worth and transactions
   to check (data that exists today).
2. **Tiers**: domain, store migration, API, MCP; tier on plan pages.
3. **Free money and the emergency fund**: domain calculation, API, MCP, Home's hero and
   breakdown. Retire the forecast's floor blocker.
4. **Money page**: this month's buckets by tier, bucket page, plan editing moved here; plan vs
   actual for past months (#120).
5. **Covers and overspend**: covers stored, sources by tier, auto-cover threshold, Needs a look,
   pace heads-up.
6. **Keep or invest and the 3-month chart**.
7. **Future** (secondary): projection, target, range, what-ifs.

Each phase ships as its own PR(s) and leaves coffer working.

## 8. Left to the builder

- Schema shapes, table names, migration details, API paths and field names (following existing
  conventions).
- How the low point is computed (§4.5).
- Exact wording beyond §3, within the wording tests' rules.
- Tier token colours, within the themes and AA.

## 9. Research summary

- YNAB: the honest "Ready to Assign" breakdown is good; the monthly assign ritual and choosing a
  source for every overspend are the chores users complain about. No rule limits where cover
  money comes from.
- Monarch: Fixed / Flexible / Non-monthly and one "flex number"; forecasting starts from the last
  12 months of real data with editable assumptions. Overspending is absorbed without saying from
  where.
- Simplifi: one live "Available to Spend", the closest existing idea to free money.
- Actual Budget: the only priority order for funding (templates with priorities).
- ProjectionLab: milestones on one net worth chart, cash-flow priorities ("top up cash to a
  target, invest the rest"); powerful but heavy to set up. Nobody shows today's choices in future
  money well.

## 10. Decisions from the builder check (my calls, binding on the plan)

These settle the gaps a builder found reading this document against the code. Where they
change something above, this section wins.

1. **Free money's window.** "Still need" is the rest of this month's Must pay, Essentials and
   Flexible buckets **plus every Must pay occurrence due in the next 30 days** not yet paid (so a
   mortgage due on the 1st is never free on the 30th). Free money counts **no future income**:
   it is about cash that exists. The 3-month projection behind keep or invest (§4.5) **does count
   planned income items**, whatever `include_income` says, since a projection without paychecks
   only falls; the forecast detail page keeps honouring `include_income` for its own totals.
2. **Covers from a goal or the emergency fund.** A cover from a Saving toward bucket is a **draw
   on the goal**: it lowers what the goal has set aside by that amount, exactly as goal-linked
   spending already does (`plan/goal.rs`), so the goal shows "$N short" until the accrual catches
   up or the target date moves. A cover from the emergency fund is a **record that changes no
   number** (the fund is stateless, §4.4); it exists so the owner's choice is visible and the
   overspend leaves Needs a look. Undo deletes the cover.
3. **Plan history.** Planned items get **effective-dated versions** of amount, category, tier and
   spending kind (one row per change, effective from a month). Past months read the version in
   force then; "change the plan from next month" adds a version; editing "from this month" is also
   offered. This fixes the past-month half of #120 and keeps a bucket's identity, covers and
   history intact. Migration writes one version per existing item, effective from its anchor.
4. **Emergency fund per currency.** The setting becomes a target per currency: each currency is
   either a fixed amount in that currency or N months of its Must pay plus Essentials. A currency
   with no target shows the "No emergency fund set" caution for that currency.
5. **No emergency fund set.** Free money is still shown (all of `before` counts as free, with the
   caution), but **the keep-or-invest suggestion is withheld** until a target is set: the
   original reason for the floor blocker (`plan/settings.rs:61-63`) was not overstating what can
   be invested, and that is preserved. Remove the forecast's `FloorNotSet` blocker deliberately,
   with the reason in the commit, and turn it into that caution.
6. **Needs a look is derived, not stored.** Covered overspends of the current month are listed
   until the month ends; no acknowledgement table. Overspends needing a source stay until covered
   or the month ends (then their excess has simply come from free money). "Not now" on the invest
   card is stored per household as the dismissed amount; the card returns when free money moves
   past it by more than the invest minimum.
7. **Bills and overspend.** A Bill's "spent" is its matched payment. Matched within today's
   ±10% window, the difference from plan is an overspend (or a saving) of that bill, taken from
   free money; Must pay buckets are never cover sources but can receive covers. A payment outside
   the window stays unmatched with today's caution; widening matching is out of scope.
8. **Bills paid early.** A bill matched to a payment (up to 7 days early, as today) needs 0 from
   the day it is matched, **in the forecast too** (`reduction.rs` currently keeps counting it
   until its date, so the payment is counted twice). Fix it in the shared reduction so free money
   and the forecast agree.
9. **Goals in free money versus the forecast.** Free money subtracts what goals have set aside so
   far (money that exists now); the 3-month projection subtracts further accrual as it happens.
   The forecast detail page stops leading with its horizon-wide "Surplus" and becomes the
   explanation of the cash projection and the keep-or-invest suggestion, so there is one headline
   number in the product.
10. **Budgets that don't repeat monthly.** Weekly budgets count their occurrences in the month, as
    today, and pace is linear over the month. Budgets repeating less often than monthly show
    planned and spent for their own period and get **no pace marker and no run-out heads-up**.
