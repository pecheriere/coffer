# Planning and forecasting from an AI agent

How to use coffer's spending plan, cash forecast and plan-versus-actual
report. Read [agent-usage.md](agent-usage.md) first for tokens, scopes,
account grants, errors and idempotency; this page assumes them. The
operations are in the OpenAPI document (`GET /api/v1/openapi.json`).

## First run

A new household has nothing a forecast needs, and the pieces depend on each
other, so set them up in this order. The token needs `admin` for step 4 and
`write` (with `all` at `write` if it has grants) for the rest. Ids in answers are
UUIDs; `$BASE` is the server's address.

1. An account: `POST $BASE/api/v1/accounts` with
   `{"name": "Checking", "kind": "checking", "currency": "USD"}`. A `checking`
   or `savings` account is `cash`, and a `credit_card` a `short_term_liability`:
   the two classes a forecast counts. The [kinds](agent-usage.md#accounts) are
   listed with the endpoint.
2. Its balance: `PUT $BASE/api/v1/accounts/<account>/balances/2026-11-14` with
   `{"minor_units": 900000}`. Without one, the forecast is blocked
   (`balance_missing`). A linked account gets its balances from syncs.
3. A category group and an `expense` category for the plan to name:
   `POST $BASE/api/v1/category-groups` with `{"name": "Housing"}`, then
   `POST $BASE/api/v1/categories` with
   `{"group": "<group>", "name": "Rent", "kind": "expense"}`. A household starts
   with no categories, and every planned item needs one.
4. The safety floor, which the forecast is blocked without (`floor_not_set`):

   ```sh
   curl -X PATCH -H "Authorization: Bearer $COFFER_TOKEN" -H "Content-Type: application/json" \
     $BASE/api/v1/household/settings \
     -d '{"floor": {"kind": "fixed", "amount": {"minor_units": 300000, "currency": "USD"}}}'
   ```

5. Planned items: `POST $BASE/api/v1/plan-items`, as in the next section.
6. The forecast: `GET $BASE/api/v1/forecast`. With the steps above and one
   bill it is `firm`, as in the firm example under
   [Status, blockers and cautions](#status-blockers-and-cautions). Date the
   balance today or yesterday: an older one raises `balance_stale`.

## The plan

The plan is what the household expects to pay and receive. Its parts are
planned items, in the three types below, and the household's forecast
settings. The forecast and the plan-versus-actual report read them; nothing
else does. Every example below uses ids written as `<name>`: real ids are
UUIDs.

Fields shared by every item:

- `name` (1 to 100 characters), `amount`, `direction`, `category`, `schedule`,
  and optionally `account_hint`, `notes` (up to 2000 characters) and `active`
  (default `true`; an inactive item is stored but counts nowhere).
- `amount` is a positive magnitude of 1 to 10^15 minor units, and `direction`
  (`outflow` or `inflow`) says which way the money goes. This differs from
  transactions and balances, where the sign carries the direction.
- `category` is required. An outflow needs an `expense` category, an inflow an
  `income` one, and a `transfer` category is refused (422 `invalid`), as is an
  archived one. A category's kind cannot change while a planned item or a
  goal-linked split uses it.
- `account_hint` is an open account the item is expected on. It decides who may
  see the item (see [Account grants](agent-usage.md#account-grants)); it does not move money.
  An item without a hint is visible to any token holding `transactions` on at
  least one account, so do not put an account's name in the name of an item you
  leave unhinted.
- Dates are written exactly `YYYY-MM-DD`, in the years 1970 to 2200. A
  datetime is a 400 `invalid`, since its day depends on a zone.
- An item's type never changes: a `PATCH` may replace its `schedule`, but the
  new one must have the same `type`.

A value out of range is a 422 `invalid`; a value of the wrong type or format
is a 400 `invalid`. At most 500 items exist; a 501st is a 409 `conflict`.

**Recurring.** A bill or a budget that repeats. The k-th occurrence is
`anchor_on` plus k times `every` `unit`s, counted from the anchor, so a
schedule never drifts. A month shorter than the anchor's day clamps to its
last day: anchoring on the 31st means the last day of every month, and a
yearly item anchored on February 29 falls on February 28 in other years.
`every` is 1 to 999, `unit` is `days`, `weeks`, `months` or `years`, and
`end_on` (optional, inclusive) is the last day an occurrence may fall on.

A recurring outflow has `spending`: `bill` (the default) counts in full;
`budget` counts all of the current month's occurrences, those already past
too, less what was spent in its category this month, floored at 0 (see
[What this month's spending does](#what-this-months-spending-does)). No other
item takes `spending`.

```sh
curl -X POST -H "Authorization: Bearer $COFFER_TOKEN" -H "Content-Type: application/json" \
  -H "Idempotency-Key: rent-2026" https://coffer.example/api/v1/plan-items -d @rent.json
```

```json
{
  "name": "Rent",
  "category": "<housing-category>",
  "amount": {"minor_units": 180000, "currency": "USD"},
  "direction": "outflow",
  "account_hint": "<checking-account>",
  "notes": "Due on the 28th",
  "schedule": {"type": "recurring", "every": 1, "unit": "months", "anchor_on": "2026-01-28"}
}
```

It answers 201 with the item, which also has `id`, `active`, `spending`,
`settlements`, `created_at` and `updated_at`, and `end_on: null` in its schedule:

```json
{
  "id": "<rent-item>",
  "name": "Rent",
  "category": "<housing-category>",
  "amount": {"minor_units": 180000, "currency": "USD"},
  "direction": "outflow",
  "spending": "bill",
  "account_hint": "<checking-account>",
  "notes": "Due on the 28th",
  "active": true,
  "schedule": {
    "type": "recurring",
    "every": 1,
    "unit": "months",
    "anchor_on": "2026-01-28",
    "end_on": null
  },
  "settlements": [],
  "created_at": "2026-11-15T12:00:00Z",
  "updated_at": "2026-11-15T12:00:00Z"
}
```

A budget differs in `spending` only:

```json
{
  "name": "Groceries",
  "category": "<groceries-category>",
  "amount": {"minor_units": 60000, "currency": "USD"},
  "direction": "outflow",
  "spending": "budget",
  "schedule": {"type": "recurring", "every": 1, "unit": "months", "anchor_on": "2026-01-20"}
}
```

**One-off.** One expected payment or receipt on a date, such as a dentist
bill. `spending` is refused (422).

```json
{
  "name": "Dentist",
  "category": "<health-category>",
  "amount": {"minor_units": 45000, "currency": "USD"},
  "direction": "outflow",
  "schedule": {"type": "one_off", "on": "2026-12-20"}
}
```

**Goal** (a sinking fund). Money the household sets aside for something due
later. `amount` is the target; the money accrues evenly from `start_on` to
`due_on` (which must be after it). A goal must be an `outflow`. What a goal
holds back at a date is what has accrued by then, less what was spent against
it, never below 0 and never more than the target not yet spent.

```json
{
  "name": "Holiday trip",
  "category": "<travel-category>",
  "amount": {"minor_units": 240000, "currency": "USD"},
  "direction": "outflow",
  "account_hint": "<savings-account>",
  "schedule": {"type": "goal", "start_on": "2026-09-01", "due_on": "2027-03-01"}
}
```

A goal that needs no more money is marked complete by sending its whole
schedule again (`PATCH`) with `completed: true`:

```json
{"schedule": {"type": "goal", "start_on": "2026-09-01", "due_on": "2027-03-01", "completed": true}}
```

Income is an `inflow` item with an `income` category, for example a salary:

```json
{
  "name": "Salary",
  "category": "<pay-category>",
  "amount": {"minor_units": 350000, "currency": "USD"},
  "direction": "inflow",
  "schedule": {"type": "recurring", "every": 1, "unit": "months", "anchor_on": "2026-01-25"}
}
```

Income does not reduce what is needed unless `include_income` is on (see
[Household settings](#household-settings)).

**A goal and a one-off for the same expense both count.** Nothing
de-duplicates them: a goal for the holiday and a one-off for the flights
would count the flights twice. Plan an expense as one or the other. The
forecast raises `goal_one_off_overlap` when it sees the likely case.

**Link the payment or the contribution, never both.** A goal paid from an account
that isn't the household's, such as tuition from a 529, may have been funded by a
contribution from checking. Linking both lowers its need twice for the same money, so
the surplus reads too high, wherever on the household's accounts the contribution
came from. Either link alone is fine. Linking both raises `goal_linked_both_sides`,
and the number stays `provisional` until one link goes. A charge counts as a side
even after its refund, so it is removing a link that clears the caution, which errs
toward holding more. The rule also covers one held account funding another, which the
forecast can't see or warn about: link one of them only.

**Goal links on splits.** Spending counts against a goal only when the split
that records it names the goal, with `goal` on the split:

```json
{"splits": [{"category": "<travel-category>", "goal": "<holiday-trip-item>", "minor_units": -50000}]}
```

sent to `PATCH /api/v1/transactions/{transaction_id}`. Linked spending lowers
what the goal still needs, and the category's own spending no longer counts it.
The goal must exist and be in the transaction's currency (a purchase in another
currency cannot be linked, which leaves that goal's need too high: the safe
direction). The split must be in an `expense` category (422 otherwise):
moving money into savings for a goal is not spending it, since the money is
still cash. A split sent without a `goal` loses its link. A completed or
inactive goal may still be named.

A goal the token cannot see (because its hint names an account the token
holds below `transactions`) reads `goal: null` on a split, and naming it is
refused like a missing goal (422 "a split names a goal that does not exist")
even on the transaction that holds the link. The link survives: a patch that
sends back a split of the same amount with no `goal`, which is all a token
that read `goal: null` can send, writes nothing and keeps it. Dividing that
split into other amounts drops the link without the token being able to
know; the audit log records it (the split list before and after, with the
goal's id in `before`).

Endpoints, all under `/api/v1`:

- `GET /plan-items` lists the items the token may see, ordered by id, as
  `{"items": [...]}` with no cursor. Inactive items are listed only with
  `include_inactive=true`.
- `POST /plan-items` (`write`, and `all` at `write` for a token with grants)
  creates an item. Answers 201. Accepts an `Idempotency-Key`.
- `GET /plan-items/{item_id}` reads one. An item the token may not see is a 404.
- `PATCH /plan-items/{item_id}` (`write`) changes the fields given and checks
  them against the item they would leave. `null` clears `account_hint` and
  `notes`. `schedule` replaces the whole schedule (a goal's `completed` goes
  back to `false` unless it is sent). `spending` is refused unless the item is
  left a recurring outflow. Changing `direction` to `inflow` drops its kind;
  changing it to `outflow` makes it a `bill` unless `spending` says otherwise.
- `DELETE /plan-items/{item_id}` (`write`) deletes an item for good (204); the
  audit log keeps what it held. A goal that splits are linked to is a 409
  `conflict`: mark it complete instead.
- `PUT` and `DELETE /plan-items/{item_id}/settlements/{month}` (`write`) settle one
  month of an item, or stop doing so: see [Settling a month](#settling-a-month).

The plan has no history: changing an item changes what the report shows for
past months too.

### Settling a month

A bill can be paid in a way the forecast never sees: a transfer between accounts,
or from an account coffer doesn't track. There is then no transaction to
recategorize, and without this the forecast would keep counting it until the month
ends. The same goes for a bill paid from an account that isn't the household's, even
when the transaction is there in the right category: that spending left no counted
cash, so it pays no bill, and recategorizing can't change that. Settle the bill for
that month. A settlement records that a recurring bill's or a one-off's occurrences in
one calendar month are `paid`, or `skipped` because they won't happen. It never
changes the item.

```sh
curl -X PUT -H "Authorization: Bearer $COFFER_TOKEN" -H "Content-Type: application/json" \
  $BASE/api/v1/plan-items/<internet-item>/settlements/2026-11 -d '{"kind": "paid"}'
```

```json
{
  "month": "2026-11",
  "kind": "paid",
  "member": "<member>",
  "agent": "<agent-token>",
  "settled_at": "2026-11-15T12:00:00Z"
}
```

`agent` is `null` when a member signed in did it. Reading the item shows the same
objects in its `settlements`, ordered by month.

- **On the owner's word, not a guess.** Settling takes money out of what is needed.
  Settle when the owner says it was paid or won't be; a bill you merely can't find a
  payment for is what `bill_not_seen` is for.
- **What may be settled:** a recurring `bill` and a one-off, going out. A budget is
  reduced by spending and a goal has links, so each answers 422, as does income.
  An item that has settlements can't become a budget or income: that `PATCH` is a
  409 `conflict` until they are deleted.
- **Changing the item:** a settlement was made on what the item then owed and when.
  A `PATCH` that really changes its `amount`, currency included, or its `schedule`
  removes its settlements from the household's current month on, so the occurrences
  count again until someone settles them again. The answer shows what is left, the
  `planned_item.update` audit entry says which went under `settlements`, and earlier
  months stay as history. Any other change, such as a name or category, keeps them.
  Moving a one-off away and back doesn't bring a settlement back.
- **Which months:** `YYYY-MM`, in the years 1970 to 2200, at most 12 calendar months
  from today's either way, and holding an occurrence of the item (422 otherwise; a
  malformed month is a 400). Future months count: a bill you know won't come in
  March can be skipped now.
- **Writing it again:** a `PUT` saying what the month already says writes nothing
  and answers with what is there, its original who and when included. Another `kind`
  replaces it. `DELETE` answers 204 whether or not the month was settled, and may
  clear any month of those years, however far from today.
- **Limit:** an item holds at most 60 settlements; another month is a 409 `conflict`
  until one is deleted.
- **Tokens:** as for any other plan write, `write` and `all` at `write`, decided
  before anything is looked up, so a token without it gets the same 403 for a hidden
  item as for a missing one. Each change is audited as `planned_item.settle` or
  `planned_item.unsettle`, with the member and the agent. A settlement goes with its
  item, whose delete entry in the audit log keeps them.
- **The forecast page** shows settled items, but acting on one is API only for now.

What it does to the forecast is in
[What this month's spending does](#what-this-months-spending-does), and to the
report in [Plan versus actual](#plan-versus-actual). A what-if reads settlements as
they stand and can't add any.

## Household settings

`GET /api/v1/household/settings` (`read`) and `PATCH` (`admin` scope, whatever
the token's grants) hold the forecast's defaults:

```json
{
  "floor": {"kind": "fixed", "amount": {"minor_units": 300000, "currency": "USD"}},
  "include_income": false,
  "default_horizon_months": 3,
  "stale_after_days": 7
}
```

- `floor` is the safety floor: cash that must stay untouched on top of the
  horizon's planned spending and goals. It is `null` until one is set, and
  **a forecast has no number while it is unset** (blocker `floor_not_set`): a
  silent floor of 0 would overstate what can be invested. Two kinds:
  `{"kind": "fixed", "amount": {...}}` (0 is allowed, and the amount carries
  its own currency) and `{"kind": "months_of_outflows", "months": 2}` (1 to 60),
  the recurring outflows, bills and budgets, that fall in that many months
  from `as_of`, per currency and before any reduction. A budget's earlier days in
  `as_of`'s month are not in it. One-offs and goals are
  not in it. A settled occurrence is: the floor says how much to hold, not what is owed. If that comes to 0 in a currency the number stays, with the
  caution `months_floor_is_zero`.
- `include_income` (default `false`) decides whether planned inflows reduce
  the cash needed.
- `default_horizon_months` (1 to 60, default 3) and `stale_after_days` (1 to
  365, default 7: how old a balance may be before the forecast flags it).

A `PATCH` takes any of the four; a field left out stays. A floor can be
replaced but never unset (`floor: null` leaves it), and a patch that changes
nothing writes nothing. A value out of range is a 422, one outside its type
(300 months) a 400. A change is audited as `household.settings`.

Below `all` at `transactions`, `floor` reads `null` (also in a `PATCH`'s
answer), the same as none set: a fixed floor's currency could show money the
token isn't granted. Do not conclude from `null` that the household has no
floor; the forecast will say.

The time zone is not a setting. The server's `COFFER_TIMEZONE` decides which
day "today" is, which is `as_of`'s default and which month is the current one.

## Forecast

The motivating question: over the next N months, how much cash does the
household need, and how much is above that?

```
cash_needed = planned_outflows + goals + floor - planned_inflows
surplus     = cash_available - cash_needed
```

over the window from `as_of` up to `end_exclusive`, `horizon_months` later.
A negative `surplus` is a shortfall. The sums are exact integers in minor
units, per currency, and currencies are never netted.

- `cash_available` is the signed sum of the latest balance on or before `as_of`
  of every counted account: an open `cash` or `short_term_liability` account
  (a credit card owes, so its balance is negative) with `include_in_forecast`.
  Invested, illiquid and long-term liability accounts never count. A closed card
  that still owes counts against the cash, with a caution. Every account is
  listed in `accounts` with `counted` and, when not, `not_counted`
  (`not_household`, `excluded_by_flag`, `closed` or `not_cash`). `not_household` means
  its `belongs_to` isn't `household`, such as a 529, a custodial account or a
  donor-advised fund, and it is the reason whatever else holds: a held account in class
  `cash` stays out too. Do nothing about it, unless it really is the household's money.
  Then ask a member, never going by the name alone, and set `belongs_to` to `household`
  (`PATCH /accounts/{id}`). `include_in_forecast: true` on it is a 422. Once moved, the
  rules above apply to it like any account: a 529 in class `invested` then reads
  `not_cash`.
- `planned_outflows` is every occurrence of an outflow in the window, reduced as
  described in [What this month's spending does](#what-this-months-spending-does).
- `goals` is what the goals hold back at `end_exclusive`.
- `planned_inflows` is the planned income in the window, counted only with
  `include_income`; `excluded_inflows` shows what was left out. Either way, income
  counts from the day after `as_of`: a balance dated `as_of` may already hold that
  day's money, so counting it could count it twice. If it hasn't arrived, the surplus
  reads low, the safe direction. The window's end stays where it is, so a monthly
  salary paid on `as_of`'s own day counts once less. The same goes for a hypothetical
  inflow.

`GET /api/v1/forecast` (`read`, and `all` at `transactions` for a token with
grants) takes `as_of` (`YYYY-MM-DD`, default the household's today),
`horizon_months` (1 to 60) and `include_income`, each defaulting to the
household's settings. The first example is a household that has just started:

```json
{
  "as_of": "2026-11-15",
  "end_exclusive": "2027-02-15",
  "horizon_months": 3,
  "include_income": false,
  "floor": null,
  "stale_after_days": 7,
  "status": "unavailable",
  "surplus": null,
  "totals": [],
  "accounts": [],
  "items": [],
  "months": [
    {"month": "2026-11", "lines": [], "partial": true},
    {"month": "2026-12", "lines": [], "partial": false},
    {"month": "2027-01", "lines": [], "partial": false},
    {"month": "2027-02", "lines": [], "partial": true}
  ],
  "blockers": [{"kind": "floor_not_set"}, {"kind": "no_counted_accounts"}],
  "cautions": [],
  "settled": []
}
```

Every number traces to its lines: `totals` has one row per currency,
`accounts` says what each balance contributed, `items` what each planned item
added (`gross`, `reduced_by`, `unpaid`, `counted`, how many `occurrences` and
the goal's own figures), and `months` the same by calendar month, with the
cash projected at each month's end. `partial` marks a month the window does
not cover whole. Reading a month's `lines`:

- `outflows` and `inflows` are the month's planned amounts. `inflows` shows
  planned income even with `include_income` off, while `projected_cash` (the
  cash available less outflows so far, plus inflows only with `include_income`)
  leaves it out, so do not add `inflows` to it yourself.
- `goal_accrual` is what goals add to their need that month, net of money
  already spent against them, and the first month holds everything that built
  up before it. It is not what the schedule alone would set aside.

`settled` lists what a person [settled](#settling-a-month), and so what is in none of
the numbers above: an entry per item and month, with `source`, `name`, `item_type`,
`category`, `month`, `kind`, how many `occurrences` and their whole `amount`. A month
the window doesn't reach changes nothing and isn't listed. `amount` is what was
planned, so a bill spending had already covered changes the number by less: its
spending goes to the category's next bill or budget instead. Settling raises no
caution, so a forecast whose only doubt was that bill is `firm`, since a person
decided. Say what was settled when you report the number.

### Status, blockers and cautions

`status` says how far to trust the number:

| `status` | Meaning |
|---|---|
| `firm` | A number, with nothing to warn about. |
| `provisional` | A number, with `cautions`: each says what might change it. |
| `unavailable` | No number: `surplus` is `null` and `blockers` say why. |

**Never report a `provisional` number without its cautions.** A provisional
`surplus` is not a figure the household can invest on its own: say what might
change it, in words, next to the number. Never report a `surplus` of 0 or a
guess when the status is `unavailable`; report the blockers and what to do.
Never sum `surplus` rows of different currencies, and never round a shortfall
to nothing.

Here is a firm forecast of a household with a floor, three accounts and one
bill that falls later (`accounts`, `items` and `months` are shortened to their
first entry):

```json
{
  "as_of": "2026-11-15",
  "end_exclusive": "2027-02-15",
  "horizon_months": 3,
  "include_income": false,
  "floor": {"kind": "fixed", "amount": {"minor_units": 300000, "currency": "USD"}},
  "stale_after_days": 7,
  "status": "firm",
  "surplus": {"minor_units": 2980000, "currency": "USD"},
  "totals": [
    {
      "currency": "USD",
      "cash_available": {"minor_units": 3820000, "currency": "USD"},
      "planned_outflows": {"minor_units": 540000, "currency": "USD"},
      "goals": {"minor_units": 0, "currency": "USD"},
      "floor": {"minor_units": 300000, "currency": "USD"},
      "planned_inflows": {"minor_units": 0, "currency": "USD"},
      "excluded_inflows": {"minor_units": 0, "currency": "USD"},
      "cash_needed": {"minor_units": 840000, "currency": "USD"},
      "surplus": {"minor_units": 2980000, "currency": "USD"}
    }
  ],
  "accounts": [
    {
      "account": "<checking-account>",
      "name": "Checking",
      "liquidity": "cash",
      "balance": {"minor_units": 900000, "currency": "USD"},
      "balance_on": "2026-11-14",
      "counted": true,
      "not_counted": null
    }
  ],
  "items": [
    {
      "source": {"kind": "planned_item", "id": "<rent-item>"},
      "name": "Rent",
      "item_type": "recurring",
      "direction": "outflow",
      "category": "<housing-category>",
      "account_hint": "<checking-account>",
      "occurrences": 3,
      "first_on": "2026-11-28",
      "last_on": "2027-01-28",
      "gross": {"minor_units": 540000, "currency": "USD"},
      "reduced_by": {"minor_units": 0, "currency": "USD"},
      "unpaid": {"minor_units": 0, "currency": "USD"},
      "counted": {"minor_units": 540000, "currency": "USD"},
      "goal": null
    }
  ],
  "months": [
    {
      "month": "2026-11",
      "partial": true,
      "lines": [
        {
          "currency": "USD",
          "outflows": {"minor_units": 180000, "currency": "USD"},
          "inflows": {"minor_units": 0, "currency": "USD"},
          "goal_accrual": {"minor_units": 0, "currency": "USD"},
          "projected_cash": {"minor_units": 3640000, "currency": "USD"},
          "items": [{"source": {"kind": "planned_item", "id": "<rent-item>"}, "amount": {"minor_units": 180000, "currency": "USD"}}]
        }
      ]
    }
  ],
  "blockers": [],
  "cautions": [],
  "settled": []
}
```

**Blockers** mean no number at all. Each has a `kind`:

| `kind` | Means | Do |
|---|---|---|
| `floor_not_set` | No safety floor. | Ask the owner what to hold back, then `PATCH /household/settings` (needs `admin`). To explore without setting one, a what-if may carry its own `floor`; say that it is not the household's. |
| `no_counted_accounts` | No open `cash` or card account is in the forecast. | Look at `accounts[].not_counted`. Create or bind the household's accounts, or set `include_in_forecast` on the right ones (`PATCH /accounts/{id}`). A flag doesn't fix this on an account with `not_household`: `true` is a 422, because that money isn't the household's cash. |
| `balance_missing` (`account`, `name`) | A counted account has no balance on or before `as_of`, so it could hold anything. | Record one (`PUT /accounts/{id}/balances/{on}`), wait for or request a sync of a linked account, or take the account out of the forecast. An account with `not_household` never raises this, and no flag makes a held account's balance count: if this one isn't the household's money, a member says so with `belongs_to`. |
| `mixed_currencies` (`currencies`) | More than one currency is in use. A forecast doesn't convert between currencies (what a household can invest is held per currency, and one converted figure would hide that), so there is no `surplus`. Conversion lives in [net worth](agent-net-worth.md), which does not lift this. | Report each row of `totals` on its own, with its own `surplus`, and never add them. A fixed floor belongs to its own currency, so another currency's row has a floor of 0; a months floor is worked out per currency from that currency's recurring outflows. |

**Cautions** keep the number but make it `provisional`. Each has a `kind`
and the fields shown:

| `kind` | Means | Do |
|---|---|---|
| `balance_stale` (`account`, `on`, `age_days`) | A counted balance is older than `stale_after_days`, its age measured to the earlier of `as_of` and today. | Sync, or ask the owner for a current balance. The cash may have changed. |
| `unbound_provider_account` (`provider_account`, `connection`, `name`, `institution`, `currency_raw`, `currency_supported`) | A provider account's money is in no account, so it is not counted. | Bind it (`POST /provider-accounts/{id}/bind`, or `create-account`), or tell the owner. It cannot be dismissed yet, which is why it does not block. It may be a 529, a custodial account or a donor-advised fund: ask a member what it is, since its name alone doesn't decide. Bound as one, it lifts this caution and the number doesn't change, as it is `not_household`. |
| `account_feed_problem` (`account`, `provider_account`, `reason`: `provider_error`, `currency_unsupported` or `currency_differs`) | A counted account's feed may not be keeping it current. | Read the provider account in `GET /provider-accounts` and tell the owner. |
| `connection_needs_attention` (`connection`, `label`, `status`: `error` or `reauth_required`) | A connection that could change the number is not syncing: it has no provider account yet, has some not yet bound, or feeds an account the number reads, meaning a counted one or one with spending linked to a goal, held or not. One that feeds only accounts the number does not read is not raised here, though it still shows as failing in `GET /connections`. | See [Connections and sync](agent-usage.md#connections-and-sync): ask for a reconnect. |
| `as_of_not_today` (`today`) | The window does not start today. What falls due before `as_of`'s month is left out; within that month a passed bill still owes its gap and a budget counts its whole month, while planned income dated on or before `as_of` and any hypothetical item dated before it are left out. | Say the date the number is for. Use `as_of` only to ask "from that day". |
| `bill_not_seen` (`category`, `amount`) | A recurring bill or a one-off dated this month, up to and including `as_of`, has not been paid, as far as the category's spending shows. The gap still counts as needed. | Ask whether it was paid. If it was paid under another category, recategorize that transaction; the caution goes. If there is no transaction to recategorize, such as a transfer or a payment from an untracked account, or the transaction is in the right category but on an account that isn't the household's (`not_counted: "not_household"`), or it won't be paid this month, [settle it](#settling-a-month) for that month: `PUT /plan-items/{id}/settlements/{month}`. See the limits below. |
| `closed_liability_owing` (`account`, `balance`) | A closed card still owes money, which counts against the cash. | If it was paid off, record its balance as 0. If it shouldn't count, set `include_in_forecast` to `false`. |
| `months_floor_is_zero` (`currency`, `months`) | A months floor comes to 0 in that currency, since no recurring outflow falls in those months. | The floor is not protecting anything. Ask for a fixed floor. |
| `liability_balance_positive` (`account`, `balance`) | A card or other short-term liability shows money in hand, often a balance entered with the wrong sign. It was added as it stands and inflates the cash. | Ask the owner and record the balance with the right sign (negative is owed). |
| `goal_one_off_overlap` (`goal`, `one_off`, `category`) | A goal and a one-off in the same category within a month of its due date may be the same expense, counted twice. A one-off settled for its month raises nothing. | Ask which it is; remove one or set it `active: false`. |
| `goal_overdue` (`goal`, `due_on`) | A goal's due date has passed with money still to find. | Ask whether it is done (`completed: true`), moved (a new `due_on`), or still owed. |
| `goal_linked_both_sides` (`goal`) | One goal has spending linked to it on both the household's accounts, counted or not, and an account that isn't the household's, such as a contribution from checking and the tuition it paid from a 529. If so, the same money lowers its need twice and the surplus is too high; the data can't tell, so the number stays. A finished goal raises nothing. | Link the payment or the contribution, never both. Ask which one, and remove the other's link. |
| `item_hint_not_counted` (`item`, `account`, `effect`) | An item is hinted at an account that is not counted (not the household's, excluded, closed or not cash). `effect` says what became of it. `still_needed`: an outflow counts anyway, since overstating the need is safe. `left_out`: an inflow into an account that doesn't count stays out of the number, since the money may never reach counted cash (an account that isn't the household's, or one left out of the forecast on purpose). The surplus can then read lower than the money you expect, and this says why. With `include_income` off, an inflow raises nothing. | For `still_needed`: fix the hint, or set the item `active: false` if that account really pays for it outside the household's cash. For `left_out`: the number is already right, so don't remove the hint just to clear the caution, since that adds the money to the number. If the money will reach counted cash, hint the item at a counted account instead. If the money isn't expected, set the item `active: false`. |
| `uncategorized_spending` (`currency`, `count`, `total`) | Uncategorized spending this month might have reduced a budget or paid a bill. | Categorize it with the [review workflow](agent-usage.md#review-workflow). |
| `possible_duplicates` (`count`) | Possible duplicates this month may inflate a category's spending, which could shrink a budget or hide an unpaid bill. | Merge or dismiss them. |
| `spending_newer_than_balance` (`currency`, `total`, `pending`) | Money went out that a balance may not show yet. | See below. |
| `cash_dips_below_floor` (`month`, `currency`, `projected_cash`) | With `include_income` on, the first month whose cash at its end is below the floor. A later inflow cannot pay an earlier bill. | Report the month. The surplus nets all the income against all the outflows, and is optimistic. |

For example, the two cautions the household below ends up with once a fuel
purchase posts:

```json
{"kind": "bill_not_seen", "category": "<utilities-category>", "amount": {"minor_units": 8000, "currency": "USD"}}
```

```json
{
  "kind": "spending_newer_than_balance",
  "currency": "USD",
  "total": {"minor_units": 6000, "currency": "USD"},
  "pending": {"minor_units": 0, "currency": "USD"}
}
```

`spending_newer_than_balance`: `total` is posted money out after the account's
balance date, up to `as_of`, and `pending` is pending money out dated up to
`as_of`, whatever the balance date. A transaction is one or the other. The
forecast assumes each balance already reflects this money, so it does not
change the number; where a balance does not (many bank balances leave out
pending charges), the cash available is overstated by up to these amounts. It
covers open accounts with a balance and any closed card you switched in.
A balance has a date and no time, so a transaction later on the balance's own
date counts as shown.

### What this month's spending does

Only a budget is reduced by what was spent. A bill or a one-off is a specific
payment, so it counts in full until its date passes; after that, spending in its
category pays it. Each category and currency has one pool of spending:

1. The month's actual spending in the category counts posted `expense` splits on
   counted accounts, on or before that account's balance date, and not linked to
   a goal. Pending charges don't count. A closed account's spending doesn't
   count, nor an excluded account's.
2. A charge on a transaction flagged `source_removed` or `split_mismatch`
   doesn't count, and neither does a goal-linked one flagged
   `possible_duplicate`: the provider took it back or changed it, so its splits
   no longer say what happened, and a bill it paid is unpaid again. A refund on
   such a transaction keeps counting. A possible duplicate in a category keeps
   counting there, with the `possible_duplicates` caution.
3. The pool first pays this month's bills and one-offs dated this month up to
   and including `as_of`, in date order. Any unpaid part is a gap: it still counts,
   with the caution `bill_not_seen`, and shows as `unpaid` on its item. What is
   left of the pool, floored at 0, reduces the budgets' occurrences in the current
   month in date order (`reduced_by`). A budget counts its whole month, whatever day
   it is anchored on, so for a stored budget `first_on`, `occurrences` and `gross`
   can include days before `as_of`, in `as_of`'s month only; later months count
   only inside the window, and never before the budget's start or after its
   `end_on`. What a budget still needs is its month's amount less the category's
   spending once bills and one-offs took their share, floored at 0; that is never
   a gap and raises no caution. So a paid bill never eats a budget, and one
   purchase never reduces two budgets. Spending in the category before a budget's
   first occurrence this month still reduces it, since a budget is a month's
   envelope for its category.
4. A bill or one-off a person [settled](#settling-a-month) for the month counts as
   nothing there, this month or a later one in the window: no `gross`, no `unpaid`,
   no `bill_not_seen`, and no share of the pool, so spending in its category goes to
   the next bill or a budget rather than being absorbed. Its other months are as
   ever.
5. A bill or one-off dated `as_of` counts in full even if it was paid that day and
   the balance already shows it: it is held twice for that day, the safe
   direction. A hypothetical item neither reduces nor consumes, a hypothetical
   budget counts only inside the window (it doesn't look back), and a goal never
   owes.

The data cannot tell where money went, so three known limits can make the
surplus too high, and the owner should check bills when the number matters:

- Spending goes to bills and one-offs before any budget, so money spent on a
  budget can hide an unpaid bill. From a bill's own date, money spent on a
  budget in its category does not reduce the budget until the bill is covered.
- The look-back is this month only: last month's unpaid bill or one-off is not
  counted, and a bill from last month that posts this month counts as paying
  this month's.
- A possible duplicate keeps counting as spending, so it can hide a gap too.

In the other direction, a bill paid under another category shows less to
invest, not more, until it is recategorized, and one paid with no transaction to
recategorize does until it is settled.

### The household's example

The examples from here on use one household. Today is 2026-11-15 and the
balances are from the day before: Checking 9,000.00, Savings 30,000.00, a Card
at -800.00. The floor is a fixed 3,000.00. The plan is the rent bill, the
groceries budget, an internet bill of 80.00 on the 10th (Utilities), the
dentist one-off, the holiday goal and the salary above. This month 250.00 was
spent in Groceries and 500.00 in Travel, linked to the goal. Here is the
forecast, with `include_income` off and arrays dropped to the parts that
matter:

```json
{
  "status": "provisional",
  "surplus": {"minor_units": 2576564, "currency": "USD"},
  "totals": [
    {
      "currency": "USD",
      "cash_available": {"minor_units": 3820000, "currency": "USD"},
      "planned_outflows": {"minor_units": 772000, "currency": "USD"},
      "goals": {"minor_units": 171436, "currency": "USD"},
      "floor": {"minor_units": 300000, "currency": "USD"},
      "planned_inflows": {"minor_units": 0, "currency": "USD"},
      "excluded_inflows": {"minor_units": 1050000, "currency": "USD"},
      "cash_needed": {"minor_units": 1243436, "currency": "USD"},
      "surplus": {"minor_units": 2576564, "currency": "USD"}
    }
  ],
  "blockers": [],
  "cautions": [
    {"kind": "bill_not_seen", "category": "<utilities-category>", "amount": {"minor_units": 8000, "currency": "USD"}}
  ]
}
```

It reads: 38,200.00 of cash, of which 25,765.64 is above the 12,434.36 the
household needs by 2027-02-15, **provisional**, because the internet bill
dated the 10th has no matching spending in Utilities (`unpaid` 80.00 on its
item line). The 7,720.00 of outflows is rent 5,400.00, groceries 1,550.00
(three budget months of 600.00 less the 250.00 already spent; its occurrences
all fall after today, so the whole-month rule changes nothing here), internet 320.00 (three
more occurrences and the unpaid one) and the dentist 450.00. The 1,714.36 for
the holiday is 2,214.36 accrued by 2027-02-15 less the 500.00 spent against it.

If the owner says the internet bill went out from an account coffer doesn't track,
[settle](#settling-a-month) November's: the unpaid 80.00 goes, so outflows are
7,640.00, the need 12,354.36 and the surplus **25,845.64**, `firm`, with the bill in
`settled`. December's and January's still count. Say that it was settled when you
report the number.

### What-if

`POST /api/v1/forecast/what-if` answers with a forecast of the same shape
for imagined changes. It persists nothing (no item, no setting, no audit
entry), so `read` may call it. A token with grants needs `all` at
`transactions`, as for the forecast. The body, every field optional:

- `items`: up to 50 hypothetical items, written like planned items except that
  `category` is optional and there is no `active` or `notes`. Each
  answers in `items` and `months` with `source: {"kind": "hypothetical",
  "index": n}`, `n` being its position in your list. This month's spending
  neither reduces nor pays them.
- `as_of`, `horizon_months`, `include_income`, `stale_after_days`: the same
  as the forecast's, overriding the household's.
- `floor`: a safety floor to use in place of the household's, such as
  `{"kind": "months_of_outflows", "months": 3}`. Without a stored floor and
  without this field the blocker `floor_not_set` still applies.

An item outside the window adds nothing. Choose a horizon that covers the date
you are testing.

Stored settlements count in a what-if as they do in the forecast, and a what-if
can't add one: its body and its items have no field for it, and a hypothetical item
never has one. To see a number without a settlement, delete it and ask again.

**Worked example: can we pay for a new roof?** The owner wants a roof for
18,000.00 in mid-March 2027. Today is 2026-11-15 and the window must reach past
March, so use six months. First, the baseline with no items:

```json
{"horizon_months": 6}
```

It answers `provisional` with a `surplus` of 18,140.00, with the cautions
`bill_not_seen` (above) and `spending_newer_than_balance`, since a 60.00 fuel
purchase posted on 2026-11-15, the day after the balances. Now the roof, paid
in full on the day:

```json
{
  "horizon_months": 6,
  "items": [
    {
      "name": "New roof",
      "amount": {"minor_units": 1800000, "currency": "USD"},
      "direction": "outflow",
      "schedule": {"type": "one_off", "on": "2027-03-15"}
    }
  ]
}
```

```json
{
  "as_of": "2026-11-15",
  "end_exclusive": "2027-05-15",
  "status": "provisional",
  "surplus": {"minor_units": 14000, "currency": "USD"},
  "totals": [
    {
      "currency": "USD",
      "cash_available": {"minor_units": 3820000, "currency": "USD"},
      "planned_outflows": {"minor_units": 3316000, "currency": "USD"},
      "goals": {"minor_units": 190000, "currency": "USD"},
      "floor": {"minor_units": 300000, "currency": "USD"},
      "planned_inflows": {"minor_units": 0, "currency": "USD"},
      "excluded_inflows": {"minor_units": 2100000, "currency": "USD"},
      "cash_needed": {"minor_units": 3806000, "currency": "USD"},
      "surplus": {"minor_units": 14000, "currency": "USD"}
    }
  ],
  "items": [
    {
      "source": {"kind": "hypothetical", "index": 0},
      "name": "New roof",
      "item_type": "one_off",
      "occurrences": 1,
      "first_on": "2027-03-15",
      "counted": {"minor_units": 1800000, "currency": "USD"}
    }
  ],
  "blockers": [],
  "cautions": [
    {"kind": "bill_not_seen", "category": "<utilities-category>", "amount": {"minor_units": 8000, "currency": "USD"}},
    {
      "kind": "spending_newer_than_balance",
      "currency": "USD",
      "total": {"minor_units": 6000, "currency": "USD"},
      "pending": {"minor_units": 0, "currency": "USD"}
    }
  ]
}
```

(`items` is cut to the roof's line, and that line to its main fields.) The roof takes the surplus from 18,140.00 to
**140.00** above the floor: nothing to invest. The same call
with the roof as a goal, `{"type": "goal", "start_on": "2026-11-15",
"due_on": "2027-03-15"}`, gives the same surplus, since the goal is due inside
the window and holds back its whole target. As a goal it also shows month by month what to set
aside (`months[].goal_accrual`, which adds up every goal, and the roof's own
entry in `months[].lines[].items`). Financing it as a recurring item, with 550.00 a month from
2027-03-15 to 2030-02-15, leaves a surplus of 17,040.00 in the same six months,
because only two of its 36 payments fall inside the window. That answers "what
do we need before May", not "can we afford the loan": use a `horizon_months`
that spans the payments, and compare it with the same call without the loan.

What to tell the owner: paying cash in March leaves 140.00 above the floor
over the next six months, which is almost nothing, and the figure is
provisional: an internet bill from the 10th has no payment in Utilities, and a
60.00 fuel charge may not be in the balance yet. Report those two with the
number, not the number alone.

## Plan versus actual

`GET /api/v1/reports/plan-vs-actual` (`read`, and `all` at `transactions` for a
token with grants) compares the plan with what happened, per category and
month: `from` (`YYYY-MM`, required), `months` (1 to 36, default 3) and
`average_months` (1 to 12, default 3: how many calendar months each rolling
average covers, ending at its row's month).

```json
{
  "rows": [
    {
      "month": "2026-11",
      "category": "<groceries-category>",
      "currency": "USD",
      "planned": {"minor_units": 60000, "currency": "USD"},
      "actual": {"minor_units": 25000, "currency": "USD"},
      "difference": {"minor_units": 35000, "currency": "USD"},
      "rolling_average": {"minor_units": 12500, "currency": "USD"},
      "left_out": {"minor_units": 0, "currency": "USD"},
      "possible_duplicate": {"minor_units": 0, "currency": "USD"},
      "settled": {"minor_units": 0, "currency": "USD"}
    }
  ],
  "goals": [
    {
      "month": "2026-11",
      "goal": "<holiday-trip-item>",
      "name": "Holiday trip",
      "currency": "USD",
      "accrued": {"minor_units": 39779, "currency": "USD"},
      "spent_against": {"minor_units": 50000, "currency": "USD"},
      "left_out": {"minor_units": 0, "currency": "USD"}
    }
  ],
  "uncategorized": [
    {"month": "2026-11", "currency": "USD", "count": 1, "total": {"minor_units": 6000, "currency": "USD"}}
  ],
  "not_the_households": [],
  "settled": [
    {
      "month": "2026-11",
      "item": "<internet-item>",
      "name": "Internet",
      "category": "<utilities-category>",
      "kind": "paid",
      "amount": {"minor_units": 8000, "currency": "USD"}
    }
  ],
  "rollovers": [
    {
      "category": "<groceries-category>",
      "currency": "USD",
      "budgeted": {"minor_units": 120000, "currency": "USD"},
      "spent": {"minor_units": 25000, "currency": "USD"},
      "rollover": {"minor_units": 95000, "currency": "USD"}
    }
  ],
  "partial_months": ["2026-11"]
}
```

(This is the query `?from=2026-10&months=2&average_months=2`, cut to one entry
per list.)

- `planned` is the occurrences of today's active items that fall in the month,
  bucketed where they fall, not spread across months. The plan has no history:
  editing an item rewrites past months. `difference` is `planned` minus `actual`.
- `actual` is posted splits on any of the household's accounts, counted or not
  (balance dates decide what a forecast may trust, not what happened): spending
  in an `expense` category, income in an `income` one, net of refunds, so a month
  of refunds is negative. `transfer` categories don't count.
- Money that isn't in `actual` is still reported. `left_out` is a charge (or a
  deposit in an `income` category) whose provider removed it or revised it away
  from its splits, at what its splits say. A refund on such a transaction keeps
  counting. `possible_duplicate` is how much of `actual` is on transactions
  flagged as possible duplicates.
- A [settled](#settling-a-month) occurrence stays in `planned`, since it is what was
  planned, so it shows in `difference` when no spending matches it. A row's `settled`
  says how much of `planned` is settled, and the list `settled` says which item, for
  which month, and whether `paid` or `skipped`, so the difference is no surprise. An
  inactive item plans nothing and shows nothing settled.
- `goals` has a line per goal and month: `accrued` is what its schedule sets
  aside, `spent_against` what was spent on it. A split linked to a goal counts
  only here, not in `rows`. A goal that is inactive but has linked money keeps its
  line. `left_out` is as above.
- `uncategorized` counts outflows with no splits. They are never spending, and
  money in invested and illiquid accounts is left out of it, so a trade or a
  sweep is neither spending nor drowns the list. A transfer, such as a move to
  savings, is not spending either: give it a `transfer` category, or leave it
  uncategorized, where it shows in `uncategorized`, and it stays out of `actual`.
  Uncategorized outflows on an account that isn't the household's, a held cash
  account too, are left out of every list.
- `not_the_households` is categorized money on accounts that aren't the household's,
  per month and currency, net and negative for money out, and only the months
  reported, for example `{"month": "2026-10", "net": {"minor_units": -30000, "currency":
  "USD"}}` for a 300.00 grant. It is never in `actual`, so a grant from a donor-advised
  fund doesn't count again after the contribution that funded it. Its `net` can be 0
  when money went in and out. Flags don't change it, `transfer` categories aren't in
  it, and a split linked to a goal stays on the goal's line.
- `rollovers` has a line per category and currency with a budget: what was
  `budgeted` over the reported months up to today's, what they `spent` while a
  budget was running once bills and one-offs dated up to and including today took
  their share,
  and the `rollover` between them (positive unspent, negative overspent). It is
  a window over the months you asked for, not a running balance: moving or
  widening `from` changes it. Today's month counts all its budget occurrences,
  including those still to come, and a refund in a month where a bill absorbs the
  spending is lost to it. A bill has no rollover, and the forecast carries nothing
  over.
- A settled bill or one-off takes no share of the month's spending in `rollovers`, as
  in the forecast: it was paid some other way, so spending in its category wasn't
  for it, and all of that goes on the budget.
- `partial_months` names the reported month that holds today, whose figures
  aren't final.

A total too large to represent is a 422.
