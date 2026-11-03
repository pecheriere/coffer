# Net worth and exchange rates from an AI agent

How to read what the household is worth, period by period, in one currency, and
how coffer converts between currencies. Read [agent-usage.md](agent-usage.md)
first for tokens, scopes, account grants and errors; this page assumes them. The
operations are in the OpenAPI document (`GET /api/v1/openapi.json`).

**A restricted token's net worth is only what it can see, and nothing says so.**
A token with account grants gets the sum of the accounts it holds at `balances`
or better. The answer is not marked partial, has no caution about it and is
`firm` when everything it does see is firm, because marking it would reveal that
other accounts exist. Check `GET /api/v1/me` before you report a figure: if
`access.all` is `null`, the token is restricted, and the sum is **not** the
household's net worth. Say "the accounts this token can see" and ask the member
for a token with an `all` grant at `balances` if the household's figure is
wanted. In the household of the example below, a token that holds only the checking
account at `balances` gets a `firm` report with `total` 500000 USD and no
cautions, where the household's is 581720: nothing in it differs from a household
of one account. A token that sees no counted account reads, outside `buckets`, the
same as a household with none (`no_counted_accounts`), which is not a net worth of
zero either. The same goes for a bucket: it sums the accounts the token holds at
`balances`, unmarked, so a token holding only the college fund below gets a `firm`
bucket for `Child A` of 2000000 USD, where a token seeing both of that bucket's
accounts gets 2111720, and `no_counted_accounts` at the top. An account a token doesn't see, or a whole bucket,
is mentioned nowhere.

## First run

A new household has no display currency and no history, so a report is blocked
until these are done. The token needs `write` (and `all` at `write` if it has
grants) for steps 1 and 2, and `admin` (and `all` at `balances` if it has
grants) for step 3. `$BASE` is the server's address; ids in answers are UUIDs.

1. An account in each currency the household holds:

   ```sh
   curl -X POST -H "Authorization: Bearer $COFFER_TOKEN" -H "Content-Type: application/json" \
     $BASE/api/v1/accounts \
     -d '{"name": "Savings", "kind": "savings", "currency": "EUR"}'
   ```

   The account's `kind` sets its liquidity class (see
   [Accounts](agent-usage.md#accounts)); a credit card is a
   `short_term_liability`. Net worth counts every account unless it is
   created with `"include_in_net_worth": false`, or isn't the household's: an
   `education`, `custodial` or `charitable` account isn't, by default, and goes in
   a [bucket](#buckets-money-that-isnt-the-households) instead.
2. A balance on each, dated the day it was true:
   `PUT $BASE/api/v1/accounts/<savings>/balances/2026-10-29` with
   `{"minor_units": 100000}`. A balance carries forward until a newer one. An
   account has no value before its first balance, and a balance dated in the
   future has none until that day. A linked account gets its balances from
   syncs. The call answers 200 with the recorded `{on, balance, source}`. Date a
   balance today or yesterday when you want a current number: only the latest
   point's balance can raise `balance_stale` (older than the household's
   `stale_after_days`, 7 by default), so backfilling history with old dates is
   fine, as long as the latest balance is recent.
3. The display currency, which every total is added up in. It is never guessed:

   ```sh
   curl -X PATCH -H "Authorization: Bearer $COFFER_TOKEN" -H "Content-Type: application/json" \
     $BASE/api/v1/household/settings -d '{"display_currency": "USD"}'
   ```

   A member can also choose it on the Household page (`/admin/household`).
4. Exchange rates need nothing from you. When the accounts and the display
   currency come to more than one currency, coffer fetches the rates of each from
   the European Central Bank, all of their history on first use. They can arrive
   within seconds of setting the display currency, or take about a minute. Check `GET $BASE/api/v1/fx/coverage` (below). Until a rate is held, a
   report has the `rate_missing` caution with the reason `not_fetched_yet`. A
   household with one currency contacts nobody.
5. The report: `GET $BASE/api/v1/reports/net-worth?from=2026-10-01&interval=month`.
   With the steps above and rates it is `firm`, as in the example below. Just
   after the server starts, or after an upgrade that adds the daily table, it can
   be `unavailable` with the blocker `rebuilding` for a moment: ask again. So can a
   household with a long history just after its display currency changes.

## The report

`GET /api/v1/reports/net-worth` needs `read`. The query, all optional:

| Parameter | Meaning |
|---|---|
| `from` | First day, written exactly `YYYY-MM-DD`, in the years 1970 to 2200. By default one year before `to` (or today, if earlier). |
| `to` | Last day, the same format. By default the household's today. No day after today is answered. |
| `interval` | `day`, `week` (ending on Sunday) or `month`, the default. |
| `account_points` | `true` by default: each account's own value at each point. `false` leaves them out. |

A `from` after `to` or after today is 422, and so is a range of more than 1,000
periods, whatever is stored. A date that is not exactly `YYYY-MM-DD` is 400. A sum
too large for 64 bits, the household's or any bucket's, is 422 for the whole report,
with the detail "the report's totals are too large to represent", for any token
that sees those accounts. It usually means a mistyped balance: tell the owner.

A **point** is the value on the last day of its period, or on the last day
available when it says `partial: true` (the month under way, or a range that
ends mid-period). `on` is that day and `period_start` the first day of the
period inside the range. A period where nothing counts has no point, so points
can skip.

Here is a firm report of a household with a checking account and a credit card in
USD and a savings account in EUR, displayed in USD. Beside it are a college fund in
USD and a UTMA in EUR, both held for `Child A`, and a donor-advised fund called
Giving, none of which is in any of the household's numbers. (The second point's
`by_liquidity` and `by_currency` are left out, as are the first two accounts, whose
points have the same shape as the savings account's, and each account's second
point.)

```json
{
  "display_currency": "USD",
  "interval": "month",
  "from": "2026-10-01",
  "to": "2026-11-02",
  "through": "2026-11-02",
  "status": "firm",
  "points": [
    {
      "on": "2026-10-31",
      "period_start": "2026-10-01",
      "partial": false,
      "total": {"minor_units": 581720, "currency": "USD"},
      "by_liquidity": [
        {"liquidity": "cash", "amount": {"minor_units": 611720, "currency": "USD"}},
        {"liquidity": "short_term_liability", "amount": {"minor_units": -30000, "currency": "USD"}}
      ],
      "by_currency": [
        {"minor_units": 100000, "currency": "EUR"},
        {"minor_units": 470000, "currency": "USD"}
      ],
      "unconverted": []
    },
    {
      "on": "2026-11-02",
      "period_start": "2026-11-01",
      "partial": true,
      "total": {"minor_units": 581720, "currency": "USD"},
      "by_liquidity": ["..."],
      "by_currency": ["..."],
      "unconverted": []
    }
  ],
  "accounts": [
    {
      "account": "<savings>",
      "name": "Savings",
      "liquidity": "cash",
      "currency": "EUR",
      "counted": true,
      "not_counted": null,
      "first_on": "2026-10-29",
      "closed_on": null,
      "points": [
        {
          "on": "2026-10-31",
          "balance": {"minor_units": 100000, "currency": "EUR"},
          "balance_on": "2026-10-29",
          "balance_source": "manual",
          "counted": true,
          "amount": {"minor_units": 111720, "currency": "USD"},
          "rate_on": "2026-10-30",
          "conversion": "converted"
        }
      ]
    }
  ],
  "blockers": [],
  "cautions": [],
  "buckets": [
    {
      "belongs_to": "someone_else",
      "beneficiary": "Child A",
      "status": "firm",
      "points": [
        {
          "on": "2026-10-31",
          "period_start": "2026-10-01",
          "partial": false,
          "total": {"minor_units": 2111720, "currency": "USD"},
          "by_liquidity": [
            {"liquidity": "invested", "amount": {"minor_units": 2111720, "currency": "USD"}}
          ],
          "by_currency": [
            {"minor_units": 100000, "currency": "EUR"},
            {"minor_units": 2000000, "currency": "USD"}
          ],
          "unconverted": []
        },
        {
          "on": "2026-11-02",
          "period_start": "2026-11-01",
          "partial": true,
          "total": {"minor_units": 2111720, "currency": "USD"},
          "by_liquidity": ["..."],
          "by_currency": ["..."],
          "unconverted": []
        }
      ],
      "accounts": [
        {
          "account": "<college-fund>",
          "name": "College fund",
          "liquidity": "invested",
          "currency": "USD",
          "counted": true,
          "not_counted": null,
          "first_on": "2026-10-26",
          "closed_on": null,
          "points": [
            {
              "on": "2026-10-31",
              "balance": {"minor_units": 2000000, "currency": "USD"},
              "balance_on": "2026-10-26",
              "balance_source": "manual",
              "counted": true,
              "amount": {"minor_units": 2000000, "currency": "USD"},
              "rate_on": null,
              "conversion": "same"
            }
          ]
        },
        {
          "account": "<utma>",
          "name": "UTMA",
          "liquidity": "invested",
          "currency": "EUR",
          "counted": true,
          "not_counted": null,
          "first_on": "2026-10-26",
          "closed_on": null,
          "points": [
            {
              "on": "2026-10-31",
              "balance": {"minor_units": 100000, "currency": "EUR"},
              "balance_on": "2026-10-26",
              "balance_source": "manual",
              "counted": true,
              "amount": {"minor_units": 111720, "currency": "USD"},
              "rate_on": "2026-10-30",
              "conversion": "converted"
            }
          ]
        }
      ],
      "blockers": [],
      "cautions": []
    },
    {
      "belongs_to": "given_away",
      "beneficiary": null,
      "status": "firm",
      "points": [
        {
          "on": "2026-10-31",
          "period_start": "2026-10-01",
          "partial": false,
          "total": {"minor_units": 500000, "currency": "USD"},
          "by_liquidity": [
            {"liquidity": "invested", "amount": {"minor_units": 500000, "currency": "USD"}}
          ],
          "by_currency": [
            {"minor_units": 500000, "currency": "USD"}
          ],
          "unconverted": []
        },
        {
          "on": "2026-11-02",
          "period_start": "2026-11-01",
          "partial": true,
          "total": {"minor_units": 500000, "currency": "USD"},
          "by_liquidity": ["..."],
          "by_currency": ["..."],
          "unconverted": []
        }
      ],
      "accounts": [
        {
          "account": "<giving>",
          "name": "Giving",
          "liquidity": "invested",
          "currency": "USD",
          "counted": true,
          "not_counted": null,
          "first_on": "2026-10-28",
          "closed_on": null,
          "points": [
            {
              "on": "2026-10-31",
              "balance": {"minor_units": 500000, "currency": "USD"},
              "balance_on": "2026-10-28",
              "balance_source": "manual",
              "counted": true,
              "amount": {"minor_units": 500000, "currency": "USD"},
              "rate_on": null,
              "conversion": "same"
            }
          ]
        }
      ],
      "blockers": [],
      "cautions": []
    }
  ]
}
```

How to read it:

- Money is integer minor units, and **negative is owed**. `total`, a class's
  `amount` and an account point's `amount` are in the display currency;
  `by_currency` and a point's `balance` are in the account's own currency.
  Never add amounts of different currencies yourself.
- `total` is the sum of the counted accounts' `amount`. Each account's amount is
  converted and rounded half-even on its own, so a breakdown always adds up to
  its total, and a total can differ by a few minor units from converting a
  one-currency sum in one go. `by_liquidity` has a line for each class holding a
  counted account.
- **`total: null` is not 0 and is not money leaving.** It means one counted
  account has no amount that day. The `unconverted` list names it and a caution
  says why; `by_currency` is still there, with no rate involved. Report the
  per-currency sums and the cause. A `by_liquidity` line is `null` the same way.
- A point's `balance_on` is the date of the snapshot the balance comes from, and
  `balance_source` is `sync` or `manual`.
- `conversion` says how an amount came about: `same` (the account is in the
  display currency), `converted`, `missing` (no rate on or before that day),
  `too_large` (it doesn't fit in 64 bits) or `no_display_currency`.
- `rate_on` is the date of the rate used: the most recent rate on or before the
  day, never a later one. A weekend or a holiday reports the Friday before. A
  conversion between two currencies neither of which is the euro goes through
  the euro and reports the older of its two dates.
- `through` is the last day the daily values hold. A job fills in each new day
  shortly after midnight in the household's time zone (`COFFER_TIMEZONE`), so
  the first minute of a day can be answered only up to yesterday, with the
  caution `to_date_not_yet_filled`. No day after `through` or after today is a
  point. It is `null` during the blocker `rebuilding`, when no day can be answered.
- `first_on` is an account's first day with a value. **A jump in the total on
  that day is the account arriving, not money**, and a history that starts late
  is not a household that was poor. `closed_on` is the day an account closed:
  **a drop on that day is the account closing, not money.** From `closed_on` an
  account counts only while its balance is negative, since a debt doesn't close
  with the card (`closed_account_owing`). On `closed_on` itself the account's point
  keeps its `amount` but says `counted: false`, and the total leaves it out.
  Reopening an account restores its days.
  Say so when you explain a change between points.
- An account can leave net worth in two ways, which mean different things. **By a
  flag:** an account with `include_in_net_worth: false` is the household's money that
  it would rather not count. It stays in `accounts`, with `counted: false` and
  `not_counted: "excluded_by_flag"`, and has no points. **By whose money it is:** an
  account whose `belongs_to` isn't `household` isn't the household's at all. It is
  listed only in its bucket (below), with its points, and is not in `accounts`.
- Closing, names, liquidity classes, `include_in_net_worth`, `belongs_to` and
  `beneficiary` are applied when you read, so changing one of them changes the whole
  history at once. An account moved out of the household leaves every past point too,
  with no date for when it happened, so a difference between two reports asked before
  and after such a change is not money moving.

### Buckets: money that isn't the household's

`buckets` holds the accounts whose `belongs_to` isn't `household` (see
[Accounts](agent-usage.md#accounts)): a 529, a custodial account, a donor-advised
fund. Money in one is held for someone else, or has been given away, as far as coffer
treats it. **Never add a bucket to `total`, or one bucket to another, and never call a
sum of them net worth.** coffer gives no such sum, on purpose: report each bucket on
its own and say whose it is.

- `accounts` is the household's alone, and so is every number beside it: `status`,
  `points`, `blockers` and `cautions` read exactly as they would with no bucket. A
  bucket's account is listed only in its bucket.
- There is one bucket per `belongs_to` and exact `beneficiary`, so `Child A` and
  `child a` are two buckets, and `beneficiary` is `null` when there is no label. They
  come with `someone_else` before `given_away`, and within one no label first, then
  labels by their bytes. `belongs_to` says how coffer treats the money, not who
  legally owns it.
- Each bucket is the same calculation as the household's over its own accounts, with
  its own `status`, `points`, `accounts`, `blockers` and `cautions`, closing and
  rounding included. **Its `status` and `cautions` are its own**: a missing rate on a
  child's account makes that bucket `provisional` and leaves the household's `firm`,
  and the reverse. So report a bucket's cautions with the bucket's number.
- `counted: true` on a bucket's account means it is in that bucket's sums. It is
  never a place in net worth.
- `buckets` is `[]` when the token sees no such account, or while the report is
  `rebuilding`, so an empty list does not say none exists. A household with only
  buckets has `accounts: []` and the blocker `no_counted_accounts`, and its buckets
  still answer.
- To count such an account as the household's, a member sets its `belongs_to` to
  `household`: asking for `include_in_net_worth: true` on it is a 422. Never do this
  from an account's name alone.

### Blockers and cautions in a bucket

A bucket's lists use the same kinds as the tables below, and each is about that
bucket's own accounts and numbers alone.

- `display_currency_not_set` is on every bucket as on the household, and a bucket's
  points still hold `by_currency`. `rebuilding` is never on a bucket, since there are none
  while it lasts.
- `no_counted_accounts` on a bucket means nothing in it has a value in this range.
  It does not mean the money is gone: look at its accounts' `first_on` and `closed_on`.
- `closed_account_owing` means the debt keeps counting in that bucket's sums, not in
  net worth. Setting `include_in_net_worth` to `false` changes nothing, since it
  already is: if it was paid off, record its balance as 0.
- `rate_missing`, `rate_stale`, `amount_too_large`, `balance_stale`,
  `to_date_not_yet_filled`, `daily_value_missing`, `nothing_counted_after_last_point`
  and `liability_balance_positive` mean what the table says, and call for the same
  step, with "counted" meaning in the bucket's sums.

### Status, blockers and cautions

`status` says how far to trust the numbers:

| `status` | Meaning |
|---|---|
| `firm` | Numbers, with nothing to warn about. |
| `provisional` | Numbers, with `cautions`: each says what might change them. |
| `unavailable` | No total you may report: `blockers` say why. |

**Never report a `provisional` number without its cautions.** Say what might
change it, in words, next to the number. Never report a total of 0 or a guess
when the status is `unavailable`; report the blockers and what to do. Never read
a null total as a drop.

**Blockers** mean no number. Each has a `kind`:

| `kind` | Means | Do |
|---|---|---|
| `display_currency_not_set` | No display currency is chosen. Points still hold `by_currency`, and `total` is `null`. | Ask the owner which currency to show, then `PATCH /household/settings` with `display_currency` (see below). Meanwhile report each of `by_currency` on its own. |
| `rebuilding` | The daily values are being rebuilt (after an upgrade or a restore, or after a change that moves too many of them at once), and there are no points. | Wait a moment and ask again. If it lasts minutes, tell the owner: the server's log says whether the rebuild failed, and `coffer rebuild-net-worth` on the host recomputes it. |
| `no_counted_accounts` | Nothing you can see of the household's counts in the range: no account, all excluded, or the whole range is before the first balance or after every account closed. | Look at `accounts[].counted`, `first_on` and `closed_on`. An empty `accounts` means nothing you can see is the household's, and `buckets` may still hold money; report those on their own, never as net worth. Otherwise add accounts and balances, or move the range. Never read it as a net worth of 0. |

Here is a report with no display currency (`accounts`, `buckets` and the second
point are left out; each class's `amount` is `null` as well):

```json
{
  "display_currency": null,
  "interval": "month",
  "from": "2026-10-01",
  "to": "2026-11-02",
  "through": "2026-11-02",
  "status": "unavailable",
  "points": [
    {
      "on": "2026-10-31",
      "period_start": "2026-10-01",
      "partial": false,
      "total": null,
      "by_liquidity": [
        {"liquidity": "cash", "amount": null},
        {"liquidity": "short_term_liability", "amount": null}
      ],
      "by_currency": [
        {"minor_units": 100000, "currency": "EUR"},
        {"minor_units": 470000, "currency": "USD"}
      ],
      "unconverted": []
    }
  ],
  "blockers": [{"kind": "display_currency_not_set"}],
  "cautions": []
}
```

**Cautions** keep the numbers but make the report `provisional`. Each has a
`kind` and the fields shown:

| `kind` | Means | Do |
|---|---|---|
| `rate_missing` (`currency`, `reason`, `first_on`, `last_on`) | From the point `first_on` to the point `last_on` that currency's rate is missing, so the accounts it converts have no amount and those periods have `total: null`. `currency` is the one whose rate is missing: for a euro account, the display currency, never the euro. | See the reasons below. Report `by_currency` meanwhile, and say that the total is unknown, not lower. |
| `rate_stale` (`currency`, `on`, `rate_on`, `age_days`) | The point `on` used a rate more than 7 days older than the day, the worst such point. A source that stopped publishing (the ECB's rouble rate ends on 2022-03-01) would otherwise be converted at its last rate for ever. The amount is included. | Say that the amount uses an old rate, from `rate_on`. Tell the owner: the currency may no longer be published. |
| `amount_too_large` (`account`, `on`) | The last point at which that account's amount didn't fit in 64 bits. Its amount and the total are `null`. | Check the account for a mistyped balance, and tell the owner. |
| `balance_stale` (`account`, `on`, `age_days`) | On the latest point, that counted account's balance dates from `on`, older than `stale_after_days`. | Sync, or ask the owner for a current balance. The total may have changed. |
| `to_date_not_yet_filled` (`through`) | The daily values stop at `through`, before the range's end or today. | Right after midnight, ask again in a minute. If it persists for hours, tell the owner: the job that fills the days is failing. Say the date the number is for. |
| `daily_value_missing` (`account`, `first_on`, `last_on`) | From the point `first_on` to the point `last_on`, an included account that had values before has none, so the total and its class are `null`, `unconverted` names it and `by_currency` leaves its balance out. **A hole in coffer's daily values, not money leaving.** | Don't report a fall. Tell the owner: `coffer rebuild-net-worth` on the host recomputes the table. Don't work around it by adding balances yourself. |
| `nothing_counted_after_last_point` (`from`, `to`) | The periods from `from` to `to` hold nothing counted, so the last point is older than the range's end and is **not the value now**: every account counted by then has closed. | Say the date of the last point, not "now". Look at the accounts' `closed_on`. If a closed account still owes money, `closed_account_owing` is also raised. Never read the last point as today's value. |
| `liability_balance_positive` (`account`, `balance`) | A liability (a credit card, a loan) shows money in hand, perhaps entered with the wrong sign. It was added as it stands, from the last point where it did. | Ask the owner whether the sign is right. A debt is negative. Correct it with `PUT /accounts/{id}/balances/{on}`. |
| `closed_account_owing` (`account`, `balance`) | A closed account still owes money, which keeps counting. | If it was paid off, record its balance as 0. If it shouldn't count, set `include_in_net_worth` to `false` (`PATCH /accounts/{id}`). |

The `reason` of a `rate_missing`:

| `reason` | Means | Do |
|---|---|---|
| `not_fetched_yet` | No fetch has been tried for the currency. | Wait a minute and ask again, then check `GET /fx/coverage`. |
| `not_published` | The source has no series for the currency. | Tell the owner. coffer can't enter a rate by hand yet, so these accounts stay unconverted: report them per currency. It is asked again every 24 hours. |
| `fetch_failing` | The last fetch failed (coverage says how). It is tried again every hour. | If it lasts, tell the owner: the host may be unable to reach the source (see below). |
| `provider_off` | The server is set up with no source of rates (`COFFER_FX_PROVIDER=none`). | Tell the owner. Report per currency. |
| `none_that_early` | The currency has rates, none on or before that day: the balance is older than the source's series. | Report per currency for those periods. |

After a change of display currency, `rate_missing` with `not_fetched_yet` is
normal for a minute: the new currency's rates have yet to be fetched.

Here is one with a yen account, last updated on 2026-10-20, whose fetch is failing
(`accounts`, `buckets` and the second point are left out):

```json
{
  "status": "provisional",
  "points": [
    {
      "on": "2026-10-31",
      "period_start": "2026-10-01",
      "partial": false,
      "total": null,
      "by_currency": [
        {"minor_units": 100000, "currency": "EUR"},
        {"minor_units": 5000000, "currency": "JPY"},
        {"minor_units": 470000, "currency": "USD"}
      ],
      "unconverted": ["<travel-yen>"]
    }
  ],
  "cautions": [
    {"kind": "rate_missing", "currency": "JPY", "reason": "fetch_failing",
     "first_on": "2026-10-31", "last_on": "2026-11-02"},
    {"kind": "balance_stale", "account": "<travel-yen>", "on": "2026-10-20", "age_days": 13}
  ]
}
```

## Who may call what

| Operation | Needs |
|---|---|
| `GET /api/v1/reports/net-worth` | `read`. A token with grants gets the sum of what it holds at `balances`, unmarked, in each bucket as at the top (above). |
| `GET /api/v1/fx/coverage`, `GET /api/v1/fx/rates` | `read`, and with grants `all` at `balances`: which rates are stored depends on the currencies in use. A 403 whose detail is "this action needs balances access to every account", whether or not any account is hidden. |
| `GET /api/v1/household/settings` | `read`. `display_currency` reads the same for every token. |
| `PATCH /api/v1/household/settings` naming `display_currency` | `admin`, and with grants `all` at `balances`, whatever the patch names, since which codes convert at once depends on the currencies in use. The same 403. |
| `PATCH /api/v1/household/settings`, any other setting | `admin` only. |
| `/net-worth`, the page | A member's session, never a token. |

An `admin` token held to one account can change the horizon or when a balance is
stale, and can't name a display currency.

## The display currency

`display_currency` is a household setting, `null` until an admin sets it. It can
be replaced but never unset (`null` leaves it). Naming the current one changes
nothing. Changing it:

- recomputes all net worth history before the call answers, converting every day
  at that day's rates (not today's), and changes no balance;
- holds the database's write lock while it does, so other writes wait for it
  for a moment;
- is audited with the old and new codes (`household.settings`).

The currency is one of the codes coffer knows. An unknown one is 400 with the detail
"the request body is not valid JSON for this operation": it reads that way because the
code isn't in the type the body is read into. The
settings answer shows all of them:

```json
{
  "floor": null,
  "include_income": false,
  "default_horizon_months": 3,
  "stale_after_days": 7,
  "display_currency": "USD"
}
```

`stale_after_days` also decides `balance_stale` here and in a forecast.

## Exchange rates

`GET /api/v1/fx/coverage` says, for each currency a fetch was tried for, how it
went (a currency never tried has no entry):

```json
{
  "items": [
    {
      "currency": "JPY",
      "status": "failing",
      "first_on": null,
      "last_on": null,
      "last_attempt_at": "2026-11-02T12:00:00Z",
      "last_success_at": null,
      "last_error": "network"
    },
    {
      "currency": "USD",
      "status": "ready",
      "first_on": "2026-10-26",
      "last_on": "2026-10-30",
      "last_attempt_at": "2026-11-02T12:00:00Z",
      "last_success_at": "2026-11-02T12:00:00Z",
      "last_error": null
    }
  ]
}
```

`status` is `ready`, `not_published` (the source has no series) or `failing`;
`last_error` is a fixed code: `unavailable` (the source answered with an error),
`malformed` (its answer wasn't what was expected, so none of it was used) or
`network`.

`GET /api/v1/fx/rates?currency=USD&from=2026-10-26&to=2026-11-02` lists a
currency's rates in a range of at most 1,000 days (both ends counted), newest
first. `rate` is units of the currency **per one euro**, exact decimal text,
never a number. A weekend or a holiday has no entry. The euro has no entries,
being 1 on every day. A range too long or inverted is 422; an unknown code or a
date not written `YYYY-MM-DD` is 400.

```json
{
  "currency": "USD",
  "items": [
    {"on": "2026-10-30", "rate": "1.1172"},
    {"on": "2026-10-29", "rate": "1.113"},
    {"on": "2026-10-28", "rate": "1.1133"},
    {"on": "2026-10-27", "rate": "1.115"},
    {"on": "2026-10-26", "rate": "1.1188"}
  ]
}
```

How coffer gets and keeps them:

- The source is the ECB's data API, one request per currency in use (an
  account's, or the display currency, the euro aside), only when those come to
  more than one currency. The first fetch takes the currency's whole history.
  After that coffer asks every 6 hours (every hour while failing, every 24
  hours when the currency isn't published), from 7 days before its last rate, so
  a missed day or a correction to the last 7 days arrives by itself. A
  correction to an older rate is not fetched. Nothing else is: the forecast
  never reads rates.
- The answer is read strictly, and all of it or none: one row that isn't exactly
  what is expected refuses the whole answer, so rates are missing rather than
  wrong. A row with an **empty `OBS_VALUE`** is a day with no rate (the ECB's series
  have these on some holidays, up to 2012), so it yields nothing and passes
  every other check. **Every line must end with its line ending**: a row cut off
  by the connection closing could read as a false rate (`1.1172` as `1.1`). No
  date may be before 1990-01-01 or the start asked for, or after tomorrow.
- **An answer replaces the days it speaks for**, from the start of its window to
  its own last row: a stored rate absent there is removed and the rows from that
  day rewritten, in one transaction, so a false rate on a weekend or holiday
  doesn't last. Rates after its last row stay, since an answer cut at the end of
  a line looks like a shorter one. An empty answer or a failure removes nothing.
- A new or corrected rate rewrites the daily values of the accounts it converts,
  from its date, in the same transaction. Nothing is audited for it: rates and
  daily values are public data and derived values, and what changes them is.
- `COFFER_FX_PROVIDER=none` asks nobody. Reports then say `provider_off` for any
  rate they lack.

## The page

A member signed in opens `/net-worth`: the same report in a table and a chart,
read-only, over the ranges 3 months (by day), 1 year (by week), 5 years and all
(by month), chosen with links (`/net-worth?range=3m|1y|5y|all`). It shows the
same cautions in words and says why a line may jump or drop without any money
moving: an account arriving at its `first_on`, or closing at its `closed_on`.
Each bucket has its own section under "Not part of your net worth", with its status,
reasons, accounts and periods. No bucket has a chart, and nothing adds one to
another or to the household's.
It needs a session, not a token, and the figures on it are as sensitive as the
API's.

## What it doesn't do

- **A forecast doesn't convert.** `mixed_currencies` still blocks a forecast
  of a household in more than one currency, whatever rates are held; see
  [agent-forecast.md](agent-forecast.md). Net worth is where conversion lives.
- **No rate can be entered by hand**, so a currency the ECB doesn't publish stays
  unconverted (`not_published`).
- One display currency for the household, not one per member.
