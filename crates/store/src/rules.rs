//! Rules, and the transactions a manual run reads (spec §8.13).

use coffer_domain::ids::{AccountId, CategoryId, MemberId, RuleId, TransactionId};
use coffer_domain::money::Currency;
use coffer_domain::policy::AccountScope;
use coffer_domain::rules::{
    AmountRange, Rule, RuleAssignee, RuleCategory, RuleMatchKind, RulePattern, RuleText,
};
use coffer_domain::transactions::MAX_POSITION;
use jiff::Timestamp;
use sqlx::query::Query;
use sqlx::sqlite::{SqliteArguments, SqliteRow};
use sqlx::{QueryBuilder, Row, Sqlite, SqliteExecutor};

use crate::convert::{corrupt, parsed, parsed_opt, text, text_opt, time};
use crate::members::expect_one;
use crate::transactions::{
    TransactionRecord, attach_children, columns, push_in, transaction_from_row,
};
use crate::{Store, StoreError, StoreTx};

macro_rules! select_rule {
    () => {
        "SELECT r.id, r.match_kind, r.pattern, r.account_id, r.amount_currency, \
         r.amount_min_minor, r.amount_max_minor, r.category_id, r.assignee_id, r.position, \
         r.enabled, r.created_at, r.updated_at, c.kind AS category_kind, \
         (c.archived_at IS NOT NULL OR g.archived_at IS NOT NULL) AS category_archived, \
         m.disabled_at IS NOT NULL AS assignee_disabled \
         FROM rules r \
         LEFT JOIN categories c ON c.id = r.category_id \
         LEFT JOIN category_groups g ON g.id = c.group_id \
         LEFT JOIN members m ON m.id = r.assignee_id"
    };
}

/// A stored rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleRecord {
    pub rule: Rule,
    pub position: u32,
    /// A rule switched off runs on no new transaction, though it can still be tried
    /// and run by hand.
    pub enabled: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// What a person chooses of a rule. `category` or `assignee` is set, or both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleFields {
    pub text: RuleText,
    pub account: Option<AccountId>,
    pub amount: Option<AmountRange>,
    pub category: Option<CategoryId>,
    pub assignee: Option<MemberId>,
    pub position: u32,
    pub enabled: bool,
}

fn rule_from_row(row: &SqliteRow) -> Result<RuleRecord, StoreError> {
    let get_i64 = |column: &str| {
        row.try_get::<Option<i64>, _>(column)
            .map_err(StoreError::from_sqlx)
    };
    let get_flag = |column: &str| {
        row.try_get::<Option<bool>, _>(column)
            .map_err(StoreError::from_sqlx)
    };
    let amount = text_opt(row, "amount_currency")?
        .map(|code| {
            let currency = Currency::parse(&code).map_err(StoreError::backend)?;
            AmountRange::new(
                currency,
                get_i64("amount_min_minor")?,
                get_i64("amount_max_minor")?,
            )
            .map_err(StoreError::backend)
        })
        .transpose()?;
    let category = parsed_opt::<CategoryId>(row, "category_id")?
        .map(|id| {
            Ok::<_, StoreError>(RuleCategory {
                id,
                kind: parsed(row, "category_kind")?,
                archived: get_flag("category_archived")?.unwrap_or(false),
            })
        })
        .transpose()?;
    let assignee = parsed_opt::<MemberId>(row, "assignee_id")?
        .map(|id| {
            Ok::<_, StoreError>(RuleAssignee {
                id,
                disabled: get_flag("assignee_disabled")?.unwrap_or(false),
            })
        })
        .transpose()?;
    let position = get_i64("position")?
        .and_then(|stored| u32::try_from(stored).ok())
        .filter(|position| *position <= MAX_POSITION)
        .ok_or_else(|| corrupt("rule position out of range"))?;
    Ok(RuleRecord {
        rule: Rule {
            id: parsed(row, "id")?,
            text: RuleText::new(
                parsed::<RuleMatchKind>(row, "match_kind")?,
                RulePattern::parse(&text(row, "pattern")?).map_err(StoreError::backend)?,
            ),
            account: parsed_opt(row, "account_id")?,
            amount,
            category,
            assignee,
        },
        position,
        enabled: get_flag("enabled")?.ok_or_else(|| corrupt("rule without enabled"))?,
        created_at: time(row, "created_at")?,
        updated_at: time(row, "updated_at")?,
    })
}

async fn fetch_rule<'e>(
    executor: impl SqliteExecutor<'e>,
    id: RuleId,
) -> Result<RuleRecord, StoreError> {
    let row = sqlx::query(concat!(select_rule!(), " WHERE r.id = ?"))
        .bind(id.to_string())
        .fetch_one(executor)
        .await
        .map_err(StoreError::from_sqlx)?;
    rule_from_row(&row)
}

async fn fetch_rules<'e>(executor: impl SqliteExecutor<'e>) -> Result<Vec<RuleRecord>, StoreError> {
    let rows = sqlx::query(concat!(select_rule!(), " ORDER BY r.position, r.id"))
        .fetch_all(executor)
        .await
        .map_err(StoreError::from_sqlx)?;
    rows.iter().map(rule_from_row).collect()
}

/// Binds `fields` and `at`, in the order the insert and the update name them.
fn bind_fields<'q>(
    query: Query<'q, Sqlite, SqliteArguments>,
    fields: &RuleFields,
    at: Timestamp,
) -> Query<'q, Sqlite, SqliteArguments> {
    query
        .bind(fields.text.kind().as_str())
        .bind(fields.text.pattern().as_str().to_owned())
        .bind(fields.account.map(|account| account.to_string()))
        .bind(fields.amount.map(|amount| amount.currency().as_str()))
        .bind(fields.amount.and_then(|amount| amount.min_minor()))
        .bind(fields.amount.and_then(|amount| amount.max_minor()))
        .bind(fields.category.map(|category| category.to_string()))
        .bind(fields.assignee.map(|assignee| assignee.to_string()))
        .bind(i64::from(fields.position))
        .bind(fields.enabled)
        .bind(at.as_millisecond())
}

impl StoreTx {
    /// Every rule, in the order they run, as seen by this transaction.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn list_rules(&mut self) -> Result<Vec<RuleRecord>, StoreError> {
        fetch_rules(&mut *self.tx).await
    }

    /// The rules that run on a new transaction, in order.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn enabled_rules(&mut self) -> Result<Vec<Rule>, StoreError> {
        let records = fetch_rules(&mut *self.tx).await?;
        Ok(records
            .into_iter()
            .filter(|record| record.enabled)
            .map(|record| record.rule)
            .collect())
    }

    /// One rule, as seen by this transaction.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such rule.
    pub async fn get_rule(&mut self, id: RuleId) -> Result<RuleRecord, StoreError> {
        fetch_rule(&mut *self.tx, id).await
    }

    /// The position after the last rule, capped like every position.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn next_rule_position(&mut self) -> Result<u32, StoreError> {
        let last: Option<i64> = sqlx::query_scalar("SELECT max(position) FROM rules")
            .fetch_one(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        let next = last.map_or(0, |last| last.saturating_add(1));
        Ok(u32::try_from(next.min(i64::from(MAX_POSITION))).unwrap_or(MAX_POSITION))
    }

    /// Creates a rule.
    ///
    /// # Errors
    ///
    /// Backend failures, including a category, account or member that does not exist.
    pub async fn insert_rule(
        &mut self,
        fields: &RuleFields,
        at: Timestamp,
    ) -> Result<RuleRecord, StoreError> {
        let id = RuleId::new();
        bind_fields(
            sqlx::query(
                "INSERT INTO rules (match_kind, pattern, account_id, amount_currency, \
                 amount_min_minor, amount_max_minor, category_id, assignee_id, position, enabled, \
                 updated_at, created_at, id) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            ),
            fields,
            at,
        )
        .bind(at.as_millisecond())
        .bind(id.to_string())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        fetch_rule(&mut *self.tx, id).await
    }

    /// Replaces what a rule holds. What it already did stays as it is.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such rule; backend failures, including a category,
    /// account or member that does not exist.
    pub async fn update_rule(
        &mut self,
        id: RuleId,
        fields: &RuleFields,
        at: Timestamp,
    ) -> Result<RuleRecord, StoreError> {
        let result = bind_fields(
            sqlx::query(
                "UPDATE rules SET match_kind = ?, pattern = ?, account_id = ?, \
                 amount_currency = ?, amount_min_minor = ?, amount_max_minor = ?, \
                 category_id = ?, assignee_id = ?, position = ?, enabled = ?, updated_at = ? \
                 WHERE id = ?",
            ),
            fields,
            at,
        )
        .bind(id.to_string())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())?;
        fetch_rule(&mut *self.tx, id).await
    }

    /// Deletes a rule. What it already did stays, still naming it.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such rule.
    pub async fn delete_rule(&mut self, id: RuleId) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM rules WHERE id = ?")
            .bind(id.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }
}

impl Store {
    /// Every rule, in the order they run.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn list_rules(&self) -> Result<Vec<RuleRecord>, StoreError> {
        fetch_rules(&self.pool).await
    }

    /// One rule.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such rule.
    pub async fn get_rule(&self, id: RuleId) -> Result<RuleRecord, StoreError> {
        fetch_rule(&self.pool, id).await
    }

    /// A page of the transactions in `scope` (and `account`, when given) with a field
    /// nobody has set, after `after` in id order. Read without the write lock, so a
    /// manual run can go through a whole history while others write.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn transactions_with_open_fields(
        &self,
        scope: &AccountScope,
        account: Option<AccountId>,
        after: Option<TransactionId>,
        limit: u32,
    ) -> Result<Vec<TransactionRecord>, StoreError> {
        if matches!(scope, AccountScope::Only(ids) if ids.is_empty()) {
            return Ok(Vec::new());
        }
        let mut query = QueryBuilder::<Sqlite>::new(concat!(
            "SELECT ",
            columns!(),
            " FROM transactions WHERE (categorized_at IS NULL \
             OR (assignee_id IS NULL AND assigned_at IS NULL))"
        ));
        if let AccountScope::Only(ids) = scope {
            push_in(&mut query, "account_id", ids);
        }
        if let Some(account) = account {
            query
                .push(" AND account_id = ")
                .push_bind(account.to_string());
        }
        if let Some(after) = after {
            query.push(" AND id > ").push_bind(after.to_string());
        }
        query
            .push(" ORDER BY id LIMIT ")
            .push_bind(i64::from(limit));
        // One read transaction, so the splits belong to the rows read.
        let mut read = self.pool.begin().await.map_err(StoreError::from_sqlx)?;
        let rows = query
            .build()
            .fetch_all(&mut *read)
            .await
            .map_err(StoreError::from_sqlx)?;
        let mut items = rows
            .iter()
            .map(transaction_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        attach_children(&mut read, &mut items).await?;
        Ok(items)
    }
}
