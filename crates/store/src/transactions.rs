//! Canonical transactions and their splits.

use coffer_domain::ids::{AccountId, AgentTokenId, CategoryId, MemberId, SplitId, TransactionId};
use coffer_domain::money::Currency;
use coffer_domain::transactions::{
    Description, Memo, Notes, Payee, ReviewStatus, SplitInput, TransactionFlag, TransactionOrigin,
    UserState,
};
use jiff::Timestamp;
use jiff::civil::Date;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite, SqliteConnection};

use crate::convert::{corrupt, parsed, parsed_opt, text, text_opt, time, time_opt};
use crate::members::expect_one;
use crate::{Store, StoreError, StoreTx};

macro_rules! columns {
    () => {
        "id, account_id, origin, posted_on, transacted_on, pending, amount_minor, currency, \
         raw_description, imported_payee, payee, payee_edited, memo, notes, review, \
         assignee_id, reviewed_by_member, reviewed_by_agent, reviewed_at, \
         categorized_by_member, categorized_by_agent, categorized_by_system, categorized_at, \
         flag_possible_duplicate, flag_source_removed, flag_split_mismatch, created_at, \
         updated_at"
    };
}

/// Who made a change and when: a member (possibly through one of their
/// agent tokens), or the system with its reason (such as `system:sync`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    pub member: Option<MemberId>,
    pub agent: Option<AgentTokenId>,
    pub system: Option<String>,
    pub at: Timestamp,
}

/// One stored split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitRecord {
    pub id: SplitId,
    pub category: CategoryId,
    pub minor_units: i64,
}

/// A stored transaction with its splits, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionRecord {
    pub id: TransactionId,
    pub account: AccountId,
    pub origin: TransactionOrigin,
    pub posted_on: Date,
    pub transacted_on: Option<Date>,
    pub pending: bool,
    pub amount_minor: i64,
    pub currency: Currency,
    pub raw_description: Description,
    /// The payee the source gave.
    pub imported_payee: Option<Payee>,
    /// The payee members see: the imported one unless `payee_edited`.
    pub payee: Option<Payee>,
    pub payee_edited: bool,
    pub memo: Option<Memo>,
    pub notes: Option<Notes>,
    pub review: ReviewStatus,
    pub assignee: Option<MemberId>,
    /// Who marked it reviewed; `Some` exactly while it is reviewed.
    pub reviewed: Option<Attribution>,
    /// Who last changed its splits.
    pub categorized: Option<Attribution>,
    /// Set flags, in [`TransactionFlag`] declaration order.
    pub flags: Vec<TransactionFlag>,
    pub splits: Vec<SplitRecord>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl TransactionRecord {
    /// Which user fields are set.
    #[must_use]
    pub fn user_state(&self) -> UserState {
        UserState {
            splits: !self.splits.is_empty(),
            notes: self.notes.is_some(),
            reviewed: self.review == ReviewStatus::Reviewed,
            assignee: self.assignee.is_some(),
            payee_edited: self.payee_edited,
        }
    }
}

/// A transaction to create, unreviewed, unassigned, uncategorized and
/// unflagged, its payee following `imported_payee`.
#[derive(Debug, Clone)]
pub struct NewTransaction {
    pub origin: TransactionOrigin,
    pub account: AccountId,
    pub posted_on: Date,
    pub transacted_on: Option<Date>,
    pub pending: bool,
    pub amount_minor: i64,
    /// The account's currency.
    pub currency: Currency,
    pub raw_description: Description,
    pub imported_payee: Option<Payee>,
    pub memo: Option<Memo>,
    /// Empty notes are stored as none.
    pub notes: Option<Notes>,
    pub created_at: Timestamp,
}

/// A change to the payee members see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayeeChange {
    /// Use this payee from now on, whatever the source says.
    Set(Payee),
    /// Follow the imported payee again.
    RestoreImported,
}

/// A change of review status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewChange {
    /// Reviewed, by whom and when.
    Reviewed(Attribution),
    /// Back to unreviewed; the reviewer is forgotten.
    Unreviewed,
}

/// The user fields of a transaction to change; `None` leaves a field as it
/// is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserFieldChanges {
    pub payee: Option<PayeeChange>,
    /// `Some(None)` clears the notes; empty notes clear them too.
    pub notes: Option<Option<Notes>>,
    pub review: Option<ReviewChange>,
    /// `Some(None)` unassigns.
    pub assignee: Option<Option<MemberId>>,
    /// When the change was made: the new `updated_at`.
    pub at: Timestamp,
}

impl UserFieldChanges {
    /// No change, made at `at`.
    #[must_use]
    pub fn new(at: Timestamp) -> Self {
        Self {
            payee: None,
            notes: None,
            review: None,
            assignee: None,
            at,
        }
    }
}

fn flag(row: &SqliteRow, column: &str) -> Result<bool, StoreError> {
    match row
        .try_get::<i64, _>(column)
        .map_err(StoreError::from_sqlx)?
    {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(corrupt("flag is neither 0 nor 1")),
    }
}

fn label<T>(
    row: &SqliteRow,
    column: &str,
    parse: impl Fn(&str) -> Result<T, coffer_domain::transactions::TransactionError>,
) -> Result<Option<T>, StoreError> {
    text_opt(row, column)?
        .map(|value| parse(&value).map_err(StoreError::backend))
        .transpose()
}

fn attribution(
    row: &SqliteRow,
    member: &str,
    agent: &str,
    system: Option<&str>,
    at: &str,
) -> Result<Option<Attribution>, StoreError> {
    let Some(at) = time_opt(row, at)? else {
        return Ok(None);
    };
    Ok(Some(Attribution {
        member: parsed_opt(row, member)?,
        agent: parsed_opt(row, agent)?,
        system: system
            .map(|column| text_opt(row, column))
            .transpose()?
            .flatten(),
        at,
    }))
}

fn transaction_from_row(row: &SqliteRow) -> Result<TransactionRecord, StoreError> {
    let mut flags = Vec::new();
    for (column, set) in [
        (
            "flag_possible_duplicate",
            TransactionFlag::PossibleDuplicate,
        ),
        ("flag_source_removed", TransactionFlag::SourceRemoved),
        ("flag_split_mismatch", TransactionFlag::SplitMismatch),
    ] {
        if flag(row, column)? {
            flags.push(set);
        }
    }
    Ok(TransactionRecord {
        id: parsed(row, "id")?,
        account: parsed(row, "account_id")?,
        origin: parsed(row, "origin")?,
        posted_on: parsed(row, "posted_on")?,
        transacted_on: parsed_opt(row, "transacted_on")?,
        pending: flag(row, "pending")?,
        amount_minor: row.try_get("amount_minor").map_err(StoreError::from_sqlx)?,
        currency: Currency::parse(&text(row, "currency")?).map_err(StoreError::backend)?,
        raw_description: Description::parse(&text(row, "raw_description")?)
            .map_err(StoreError::backend)?,
        imported_payee: label(row, "imported_payee", Payee::parse)?,
        payee: label(row, "payee", Payee::parse)?,
        payee_edited: flag(row, "payee_edited")?,
        memo: label(row, "memo", Memo::parse)?,
        notes: label(row, "notes", Notes::parse)?,
        review: parsed(row, "review")?,
        assignee: parsed_opt(row, "assignee_id")?,
        reviewed: attribution(
            row,
            "reviewed_by_member",
            "reviewed_by_agent",
            None,
            "reviewed_at",
        )?,
        categorized: attribution(
            row,
            "categorized_by_member",
            "categorized_by_agent",
            Some("categorized_by_system"),
            "categorized_at",
        )?,
        flags,
        splits: Vec::new(),
        created_at: time(row, "created_at")?,
        updated_at: time(row, "updated_at")?,
    })
}

async fn fetch_transaction(
    conn: &mut SqliteConnection,
    id: TransactionId,
) -> Result<TransactionRecord, StoreError> {
    let row = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM transactions WHERE id = ?"
    ))
    .bind(id.to_string())
    .fetch_one(&mut *conn)
    .await
    .map_err(StoreError::from_sqlx)?;
    let mut record = transaction_from_row(&row)?;
    let rows = sqlx::query(
        "SELECT id, category_id, minor_units FROM transaction_splits \
         WHERE transaction_id = ? ORDER BY position",
    )
    .bind(id.to_string())
    .fetch_all(&mut *conn)
    .await
    .map_err(StoreError::from_sqlx)?;
    record.splits = rows
        .iter()
        .map(|row| {
            Ok(SplitRecord {
                id: parsed(row, "id")?,
                category: parsed(row, "category_id")?,
                minor_units: row.try_get("minor_units").map_err(StoreError::from_sqlx)?,
            })
        })
        .collect::<Result<_, StoreError>>()?;
    Ok(record)
}

/// Empty notes are stored as `NULL`.
fn stored_notes(notes: Option<&Notes>) -> Option<&str> {
    notes.map(Notes::as_str).filter(|notes| !notes.is_empty())
}

impl StoreTx {
    /// Create a transaction, manual or imported.
    ///
    /// # Errors
    ///
    /// Backend failures, including an account that does not exist.
    pub async fn insert_transaction(
        &mut self,
        new: NewTransaction,
    ) -> Result<TransactionRecord, StoreError> {
        let id = TransactionId::new();
        let at = new.created_at.as_millisecond();
        let imported_payee = new.imported_payee.as_ref().map(Payee::as_str);
        sqlx::query(
            "INSERT INTO transactions (id, account_id, origin, posted_on, transacted_on, \
             pending, amount_minor, currency, raw_description, imported_payee, payee, \
             payee_edited, memo, notes, review, flag_possible_duplicate, flag_source_removed, \
             flag_split_mismatch, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?, 'unreviewed', 0, 0, 0, ?, ?)",
        )
        .bind(id.to_string())
        .bind(new.account.to_string())
        .bind(new.origin.as_str())
        .bind(new.posted_on.to_string())
        .bind(new.transacted_on.map(|day| day.to_string()))
        .bind(new.pending)
        .bind(new.amount_minor)
        .bind(new.currency.as_str())
        .bind(new.raw_description.as_str())
        .bind(imported_payee)
        .bind(imported_payee)
        .bind(new.memo.as_ref().map(Memo::as_str))
        .bind(stored_notes(new.notes.as_ref()))
        .bind(at)
        .bind(at)
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        fetch_transaction(&mut self.tx, id).await
    }

    /// Apply `changes` to a transaction's user fields and return it as it
    /// now is.
    ///
    /// # Errors
    ///
    /// `NotFound` if the transaction does not exist; backend failures,
    /// including an assignee or reviewer that does not exist.
    pub async fn update_user_fields(
        &mut self,
        id: TransactionId,
        changes: UserFieldChanges,
    ) -> Result<TransactionRecord, StoreError> {
        let mut query = QueryBuilder::<Sqlite>::new("UPDATE transactions SET updated_at = ");
        query.push_bind(changes.at.as_millisecond());
        match &changes.payee {
            Some(PayeeChange::Set(payee)) => {
                query
                    .push(", payee = ")
                    .push_bind(payee.as_str().to_owned())
                    .push(", payee_edited = 1");
            }
            Some(PayeeChange::RestoreImported) => {
                query.push(", payee = imported_payee, payee_edited = 0");
            }
            None => {}
        }
        if let Some(notes) = &changes.notes {
            query
                .push(", notes = ")
                .push_bind(stored_notes(notes.as_ref()).map(str::to_owned));
        }
        match &changes.review {
            Some(ReviewChange::Reviewed(by)) => {
                query
                    .push(", review = 'reviewed', reviewed_by_member = ")
                    .push_bind(by.member.map(|member| member.to_string()))
                    .push(", reviewed_by_agent = ")
                    .push_bind(by.agent.map(|agent| agent.to_string()))
                    .push(", reviewed_at = ")
                    .push_bind(by.at.as_millisecond());
            }
            Some(ReviewChange::Unreviewed) => {
                query.push(
                    ", review = 'unreviewed', reviewed_by_member = NULL, \
                     reviewed_by_agent = NULL, reviewed_at = NULL",
                );
            }
            None => {}
        }
        if let Some(assignee) = changes.assignee {
            query
                .push(", assignee_id = ")
                .push_bind(assignee.map(|member| member.to_string()));
        }
        query.push(" WHERE id = ").push_bind(id.to_string());
        let result = query
            .build()
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())?;
        fetch_transaction(&mut self.tx, id).await
    }

    /// Replace a transaction's splits with `splits`, in that order, and
    /// record `by` as who categorized it. The caller has checked them with
    /// [`coffer_domain::transactions::validate_splits`].
    ///
    /// # Errors
    ///
    /// `NotFound` if the transaction does not exist; backend failures,
    /// including a category that does not exist.
    pub async fn replace_splits(
        &mut self,
        id: TransactionId,
        splits: &[SplitInput],
        by: Attribution,
    ) -> Result<(), StoreError> {
        let result = sqlx::query(
            "UPDATE transactions SET categorized_by_member = ?, categorized_by_agent = ?, \
             categorized_by_system = ?, categorized_at = ?, updated_at = ? WHERE id = ?",
        )
        .bind(by.member.map(|member| member.to_string()))
        .bind(by.agent.map(|agent| agent.to_string()))
        .bind(by.system.as_deref())
        .bind(by.at.as_millisecond())
        .bind(by.at.as_millisecond())
        .bind(id.to_string())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())?;
        sqlx::query("DELETE FROM transaction_splits WHERE transaction_id = ?")
            .bind(id.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        for (position, split) in (0_i64..).zip(splits) {
            sqlx::query(
                "INSERT INTO transaction_splits \
                 (id, transaction_id, category_id, minor_units, position) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(SplitId::new().to_string())
            .bind(id.to_string())
            .bind(split.category.to_string())
            .bind(split.minor_units)
            .bind(position)
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        }
        Ok(())
    }

    /// Delete a transaction and its splits.
    ///
    /// # Errors
    ///
    /// `NotFound` if the transaction does not exist.
    pub async fn delete_transaction(&mut self, id: TransactionId) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM transactions WHERE id = ?")
            .bind(id.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// Fetch a transaction with its splits, as seen by this transaction.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such transaction.
    pub async fn get_transaction(
        &mut self,
        id: TransactionId,
    ) -> Result<TransactionRecord, StoreError> {
        fetch_transaction(&mut self.tx, id).await
    }
}

impl Store {
    /// Fetch a transaction with its splits.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such transaction.
    pub async fn get_transaction(
        &self,
        id: TransactionId,
    ) -> Result<TransactionRecord, StoreError> {
        // One read transaction, so the splits belong to the row read.
        let mut read = self.pool.begin().await.map_err(StoreError::from_sqlx)?;
        fetch_transaction(&mut read, id).await
    }
}
