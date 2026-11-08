//! Canonical transactions and their splits.

mod count;

use coffer_domain::ids::{
    AccountId, AgentTokenId, CategoryId, MemberId, PlannedItemId, RuleId, SplitId, TransactionId,
};
use coffer_domain::money::Currency;
use coffer_domain::policy::AccountScope;
use coffer_domain::rules::{RuleSubject, TransferLeg};
use coffer_domain::transactions::{
    Description, Memo, Notes, Payee, ReviewStatus, SplitInput, TransactionFlag, TransactionOrigin,
    UserState,
};
use std::collections::HashMap;

use jiff::Timestamp;
use jiff::civil::Date;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite, SqliteConnection};

use crate::convert::{corrupt, parsed, parsed_opt, text, text_opt, time, time_opt};
use crate::members::expect_one;
use crate::sources::{SourceRecord, candidates_by_transaction, sources_by_transaction};
use crate::transfer_links::{TransferLinkSide, sides_by_transaction};
use crate::{Page, PageRequest, Store, StoreError, StoreTx};

macro_rules! columns {
    () => {
        "id, account_id, origin, posted_on, transacted_on, pending, amount_minor, currency, \
         raw_description, imported_payee, payee, payee_edited, memo, notes, review, \
         assignee_id, reviewed_by_member, reviewed_by_agent, reviewed_at, \
         categorized_by_member, categorized_by_agent, categorized_by_system, \
         categorized_by_rule, categorized_at, assigned_by_member, assigned_by_agent, \
         assigned_by_system, assigned_by_rule, assigned_at, flag_possible_duplicate, flag_source_removed, flag_split_mismatch, created_at, \
         updated_at"
    };
}
pub(crate) use columns;

/// Who made a change and when: a member (possibly through one of their
/// agent tokens), or the system with its reason (such as `system:sync`).
/// `rule` names the rule that chose the value, whoever ran it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    pub member: Option<MemberId>,
    pub agent: Option<AgentTokenId>,
    pub system: Option<String>,
    pub rule: Option<RuleId>,
    pub at: Timestamp,
}

/// One stored split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitRecord {
    pub id: SplitId,
    pub category: CategoryId,
    pub minor_units: i64,
    pub goal: Option<PlannedItemId>,
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
    /// Who last set or cleared its assignee.
    pub assigned: Option<Attribution>,
    /// Who marked it reviewed; `Some` exactly while it is reviewed.
    pub reviewed: Option<Attribution>,
    /// Who last changed its splits.
    pub categorized: Option<Attribution>,
    /// Set flags, in [`TransactionFlag`] declaration order.
    pub flags: Vec<TransactionFlag>,
    pub splits: Vec<SplitRecord>,
    /// The provider records it was imported from, oldest posting day first;
    /// empty for a manual transaction.
    pub sources: Vec<SourceRecord>,
    /// The transactions it may duplicate, when flagged
    /// [`TransactionFlag::PossibleDuplicate`].
    pub duplicate_candidates: Vec<TransactionId>,
    /// The transfer link it is one leg of.
    pub transfer_link: Option<TransferLinkSide>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl TransactionRecord {
    /// Which user fields are set. Clearing a field is someone saying it stays empty, so
    /// that counts. What only a rule chose isn't anyone's work, so a sync may still
    /// discard it and a merge doesn't ask before dropping it.
    #[must_use]
    pub fn user_state(&self) -> UserState {
        let chosen = |attribution: &Option<Attribution>, filled: bool| {
            attribution.as_ref().map_or(filled, |by| by.rule.is_none())
        };
        UserState {
            splits: chosen(&self.categorized, !self.splits.is_empty()),
            notes: self.notes.is_some(),
            reviewed: self.review == ReviewStatus::Reviewed,
            assignee: chosen(&self.assigned, self.assignee.is_some()),
            payee_edited: self.payee_edited,
            transfer_link: self.transfer_link.is_some(),
        }
    }

    /// What rules read of it.
    #[must_use]
    pub fn rule_subject(&self) -> RuleSubject<'_> {
        RuleSubject {
            account: self.account,
            amount_minor: self.amount_minor,
            currency: self.currency,
            description: self.raw_description.as_str(),
            category_open: self.splits.is_empty() && self.categorized.is_none(),
            assignee_open: self.assignee.is_none() && self.assigned.is_none(),
            splits: self.splits.len(),
            reviewed: self.review == ReviewStatus::Reviewed,
            transfer_leg: match &self.transfer_link {
                None => TransferLeg::NotALeg,
                Some(side) if side.one_movement && side.one_movement_in_reports => {
                    TransferLeg::OneMovement
                }
                Some(_) => TransferLeg::OtherLink,
            },
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

/// A change of assignee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssigneeChange {
    /// `None` unassigns.
    pub member: Option<MemberId>,
    pub by: Attribution,
}

/// The user fields of a transaction to change; `None` leaves a field as it
/// is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserFieldChanges {
    pub payee: Option<PayeeChange>,
    /// `Some(None)` clears the notes; empty notes clear them too.
    pub notes: Option<Option<Notes>>,
    pub review: Option<ReviewChange>,
    pub assignee: Option<AssigneeChange>,
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

/// Whose transactions a listing returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssigneeFilter {
    /// Those assigned to this member.
    Member(MemberId),
    /// Those assigned to nobody.
    Unassigned,
}

/// Which transactions a listing returns: those matching every criterion
/// set. An empty list or `None` does not filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionFilter {
    /// The accounts the requester may see; nothing outside them is listed.
    pub scope: AccountScope,
    /// Only these accounts.
    pub accounts: Vec<AccountId>,
    /// Posted on or after this day.
    pub from: Option<Date>,
    /// Posted before this day.
    pub to: Option<Date>,
    pub review: Option<ReviewStatus>,
    pub assignee: Option<AssigneeFilter>,
    /// With at least one split in one of these categories.
    pub categories: Vec<CategoryId>,
    /// Only transactions without splits; `false` does not filter.
    pub uncategorized: bool,
    /// Only transactions with a split in a `transfer` category and in no transfer link;
    /// `false` does not filter.
    pub unmatched_transfer: bool,
    /// Filled by this rule, category or assignee, and not changed by anyone since.
    pub rule: Option<RuleId>,
    pub pending: Option<bool>,
    /// With this flag set.
    pub flag: Option<TransactionFlag>,
    /// A signed amount of at least this many minor units.
    pub min_minor: Option<i64>,
    /// A signed amount of at most this many minor units.
    pub max_minor: Option<i64>,
    /// Contained in the description, the payee or the notes, ignoring the
    /// case of ASCII letters only; `%`, `_` and `\` match themselves.
    pub text: Option<String>,
}

/// The column holding `flag`.
pub(crate) fn flag_column(flag: TransactionFlag) -> &'static str {
    match flag {
        TransactionFlag::PossibleDuplicate => "flag_possible_duplicate",
        TransactionFlag::SourceRemoved => "flag_source_removed",
        TransactionFlag::SplitMismatch => "flag_split_mismatch",
    }
}

/// A `LIKE` pattern matching any text containing `text`, its wildcards and
/// escape character escaped with `\`.
fn contains_pattern(text: &str) -> String {
    let mut pattern = String::with_capacity(text.len() + 2);
    pattern.push('%');
    for c in text.chars() {
        if matches!(c, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('%');
    pattern
}

/// The cursor continuing after `record`: `<posted_on>~<id>`.
fn cursor(record: &TransactionRecord) -> String {
    format!("{}~{}", record.posted_on, record.id)
}

fn parse_cursor(cursor: &str) -> Result<(Date, TransactionId), StoreError> {
    let (day, id) = cursor.split_once('~').ok_or(StoreError::InvalidCursor)?;
    let day = day.parse::<Date>().map_err(|_| StoreError::InvalidCursor)?;
    let id = id
        .parse::<TransactionId>()
        .map_err(|_| StoreError::InvalidCursor)?;
    Ok((day, id))
}

/// Appends ` AND <column> IN (<ids>)` to `query`.
pub(crate) fn push_in<T: std::fmt::Display>(
    query: &mut QueryBuilder<Sqlite>,
    column: &str,
    ids: &[T],
) {
    query.push(" AND ").push(column).push(" IN (");
    let mut separated = query.separated(", ");
    for id in ids {
        separated.push_bind(id.to_string());
    }
    separated.push_unseparated(")");
}

/// Appends the conditions of `filter` to `query`.
fn push_filter(query: &mut QueryBuilder<Sqlite>, filter: &TransactionFilter) {
    if let AccountScope::Only(ids) = &filter.scope {
        push_in(query, "account_id", ids);
    }
    if !filter.accounts.is_empty() {
        push_in(query, "account_id", &filter.accounts);
    }
    if let Some(from) = filter.from {
        query.push(" AND posted_on >= ").push_bind(from.to_string());
    }
    if let Some(to) = filter.to {
        query.push(" AND posted_on < ").push_bind(to.to_string());
    }
    if let Some(review) = filter.review {
        query.push(" AND review = ").push_bind(review.as_str());
    }
    match filter.assignee {
        Some(AssigneeFilter::Member(member)) => {
            query
                .push(" AND assignee_id = ")
                .push_bind(member.to_string());
        }
        Some(AssigneeFilter::Unassigned) => {
            query.push(" AND assignee_id IS NULL");
        }
        None => {}
    }
    if !filter.categories.is_empty() {
        query.push(
            " AND EXISTS (SELECT 1 FROM transaction_splits s \
             WHERE s.transaction_id = transactions.id",
        );
        push_in(query, "s.category_id", &filter.categories);
        query.push(")");
    }
    if filter.uncategorized {
        query.push(
            " AND NOT EXISTS (SELECT 1 FROM transaction_splits s \
             WHERE s.transaction_id = transactions.id)",
        );
    }
    if let Some(rule) = filter.rule {
        query
            .push(" AND (categorized_by_rule = ")
            .push_bind(rule.to_string())
            .push(" OR assigned_by_rule = ")
            .push_bind(rule.to_string())
            .push(")");
    }
    if filter.unmatched_transfer {
        query.push(
            " AND EXISTS (SELECT 1 FROM transaction_splits s \
             JOIN categories c ON c.id = s.category_id \
             WHERE s.transaction_id = transactions.id AND c.kind = 'transfer') \
             AND NOT EXISTS (SELECT 1 FROM transfer_links l \
             WHERE l.outflow_id = transactions.id OR l.inflow_id = transactions.id)",
        );
    }
    if let Some(pending) = filter.pending {
        query.push(" AND pending = ").push_bind(pending);
    }
    if let Some(flag) = filter.flag {
        query.push(" AND ").push(flag_column(flag)).push(" = 1");
    }
    if let Some(min) = filter.min_minor {
        query.push(" AND amount_minor >= ").push_bind(min);
    }
    if let Some(max) = filter.max_minor {
        query.push(" AND amount_minor <= ").push_bind(max);
    }
    if let Some(text) = &filter.text {
        let pattern = contains_pattern(text);
        query.push(" AND (raw_description LIKE ");
        query.push_bind(pattern.clone());
        query.push(" ESCAPE '\\' OR payee LIKE ");
        query.push_bind(pattern.clone());
        query.push(" ESCAPE '\\' OR notes LIKE ");
        query.push_bind(pattern);
        query.push(" ESCAPE '\\')");
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

/// The columns of one [`Attribution`].
struct AttributionColumns {
    member: &'static str,
    agent: &'static str,
    system: Option<&'static str>,
    rule: Option<&'static str>,
    at: &'static str,
}

fn attribution(
    row: &SqliteRow,
    columns: &AttributionColumns,
) -> Result<Option<Attribution>, StoreError> {
    let AttributionColumns {
        member,
        agent,
        system,
        rule,
        at,
    } = *columns;
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
        rule: rule
            .map(|column| parsed_opt(row, column))
            .transpose()?
            .flatten(),
        at,
    }))
}

pub(crate) fn transaction_from_row(row: &SqliteRow) -> Result<TransactionRecord, StoreError> {
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
        assigned: attribution(
            row,
            &AttributionColumns {
                member: "assigned_by_member",
                agent: "assigned_by_agent",
                system: Some("assigned_by_system"),
                rule: Some("assigned_by_rule"),
                at: "assigned_at",
            },
        )?,
        reviewed: attribution(
            row,
            &AttributionColumns {
                member: "reviewed_by_member",
                agent: "reviewed_by_agent",
                system: None,
                rule: None,
                at: "reviewed_at",
            },
        )?,
        categorized: attribution(
            row,
            &AttributionColumns {
                member: "categorized_by_member",
                agent: "categorized_by_agent",
                system: Some("categorized_by_system"),
                rule: Some("categorized_by_rule"),
                at: "categorized_at",
            },
        )?,
        flags,
        splits: Vec::new(),
        sources: Vec::new(),
        duplicate_candidates: Vec::new(),
        transfer_link: None,
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
    let mut records = vec![transaction_from_row(&row)?];
    attach_children(conn, &mut records).await?;
    let [record] = <[TransactionRecord; 1]>::try_from(records)
        .map_err(|_| corrupt("one transaction became several"))?;
    Ok(record)
}

/// Loads the splits (in order), source records, duplicate candidates and transfer link of
/// every record in `records`.
pub(crate) async fn attach_children(
    conn: &mut SqliteConnection,
    records: &mut [TransactionRecord],
) -> Result<(), StoreError> {
    if records.is_empty() {
        return Ok(());
    }
    let mut query = QueryBuilder::<Sqlite>::new(
        "SELECT transaction_id, id, category_id, minor_units, goal_id FROM transaction_splits \
         WHERE transaction_id IN (",
    );
    let mut separated = query.separated(", ");
    for record in records.iter() {
        separated.push_bind(record.id.to_string());
    }
    separated.push_unseparated(") ORDER BY transaction_id, position");
    let rows = query
        .build()
        .fetch_all(&mut *conn)
        .await
        .map_err(StoreError::from_sqlx)?;
    let mut splits: HashMap<TransactionId, Vec<SplitRecord>> = HashMap::new();
    for row in &rows {
        splits
            .entry(parsed(row, "transaction_id")?)
            .or_default()
            .push(SplitRecord {
                id: parsed(row, "id")?,
                category: parsed(row, "category_id")?,
                minor_units: row.try_get("minor_units").map_err(StoreError::from_sqlx)?,
                goal: parsed_opt(row, "goal_id")?,
            });
    }
    let ids: Vec<TransactionId> = records.iter().map(|record| record.id).collect();
    let mut sources = sources_by_transaction(conn, &ids).await?;
    let mut candidates = candidates_by_transaction(conn, &ids).await?;
    let mut links = sides_by_transaction(conn, &ids).await?;
    for record in records {
        record.splits = splits.remove(&record.id).unwrap_or_default();
        record.sources = sources.remove(&record.id).unwrap_or_default();
        record.duplicate_candidates = candidates.remove(&record.id).unwrap_or_default();
        record.transfer_link = links.remove(&record.id);
    }
    Ok(())
}

/// Empty notes are stored as `NULL`.
fn stored_notes(notes: Option<&Notes>) -> Option<&str> {
    notes.map(Notes::as_str).filter(|notes| !notes.is_empty())
}

async fn accounts_of(
    conn: &mut SqliteConnection,
    ids: &[TransactionId],
) -> Result<HashMap<TransactionId, AccountId>, StoreError> {
    let mut accounts = HashMap::new();
    if ids.is_empty() {
        return Ok(accounts);
    }
    let mut query =
        QueryBuilder::<Sqlite>::new("SELECT id, account_id FROM transactions WHERE id IN (");
    let mut separated = query.separated(", ");
    for id in ids {
        separated.push_bind(id.to_string());
    }
    separated.push_unseparated(")");
    let rows = query
        .build()
        .fetch_all(&mut *conn)
        .await
        .map_err(StoreError::from_sqlx)?;
    for row in &rows {
        accounts.insert(parsed(row, "id")?, parsed(row, "account_id")?);
    }
    Ok(accounts)
}

impl StoreTx {
    /// The accounts of `ids`, as [`Store::accounts_of_transactions`], read
    /// in this transaction.
    ///
    /// # Errors
    ///
    /// As [`Store::accounts_of_transactions`].
    pub async fn accounts_of_transactions(
        &mut self,
        ids: &[TransactionId],
    ) -> Result<HashMap<TransactionId, AccountId>, StoreError> {
        accounts_of(&mut self.tx, ids).await
    }

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
        if let Some(AssigneeChange { member, by }) = changes.assignee {
            query
                .push(", assignee_id = ")
                .push_bind(member.map(|member| member.to_string()))
                .push(", assigned_by_member = ")
                .push_bind(by.member.map(|member| member.to_string()))
                .push(", assigned_by_agent = ")
                .push_bind(by.agent.map(|agent| agent.to_string()))
                .push(", assigned_by_system = ")
                .push_bind(by.system)
                .push(", assigned_by_rule = ")
                .push_bind(by.rule.map(|rule| rule.to_string()))
                .push(", assigned_at = ")
                .push_bind(by.at.as_millisecond());
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
    /// including a category or goal that does not exist.
    pub async fn replace_splits(
        &mut self,
        id: TransactionId,
        splits: &[SplitInput],
        by: Attribution,
    ) -> Result<(), StoreError> {
        let result = sqlx::query(
            "UPDATE transactions SET categorized_by_member = ?, categorized_by_agent = ?, \
             categorized_by_system = ?, categorized_by_rule = ?, categorized_at = ?, \
             updated_at = ? WHERE id = ?",
        )
        .bind(by.member.map(|member| member.to_string()))
        .bind(by.agent.map(|agent| agent.to_string()))
        .bind(by.system.as_deref())
        .bind(by.rule.map(|rule| rule.to_string()))
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
                 (id, transaction_id, category_id, minor_units, position, goal_id) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(SplitId::new().to_string())
            .bind(id.to_string())
            .bind(split.category.to_string())
            .bind(split.minor_units)
            .bind(position)
            .bind(split.goal.map(|goal| goal.to_string()))
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        }
        Ok(())
    }

    /// Takes back what a rule filed: no splits, and nobody having categorized it, so the
    /// field is open again and a mismatch flag has nothing left to say.
    ///
    /// # Errors
    ///
    /// `NotFound` if the transaction does not exist; backend failures.
    pub async fn reopen_category(
        &mut self,
        id: TransactionId,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        let result = sqlx::query(
            "UPDATE transactions SET categorized_by_member = NULL, categorized_by_agent = NULL, \
             categorized_by_system = NULL, categorized_by_rule = NULL, categorized_at = NULL, \
             flag_split_mismatch = 0, updated_at = ? WHERE id = ?",
        )
        .bind(at.as_millisecond())
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
        Ok(())
    }

    /// Takes back who a rule assigned: nobody, and nobody having chosen, so the field is
    /// open again.
    ///
    /// # Errors
    ///
    /// `NotFound` if the transaction does not exist; backend failures.
    pub async fn reopen_assignee(
        &mut self,
        id: TransactionId,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        let result = sqlx::query(
            "UPDATE transactions SET assignee_id = NULL, assigned_by_member = NULL, \
             assigned_by_agent = NULL, assigned_by_system = NULL, assigned_by_rule = NULL, \
             assigned_at = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(at.as_millisecond())
        .bind(id.to_string())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// Delete a transaction and its splits.
    ///
    /// # Errors
    ///
    /// `NotFound` if the transaction does not exist; a backend failure if a transfer link
    /// still holds it, since only a person ends one.
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
    /// The accounts of `ids`, for the ones that exist.
    ///
    /// # Errors
    ///
    /// Backend failures, or a stored id that no longer parses.
    pub async fn accounts_of_transactions(
        &self,
        ids: &[TransactionId],
    ) -> Result<HashMap<TransactionId, AccountId>, StoreError> {
        let mut conn = self.pool.acquire().await.map_err(StoreError::from_sqlx)?;
        accounts_of(&mut conn, ids).await
    }

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

    /// The transactions matching `filter`, newest posting day first (ties
    /// by id, newest first), each with its splits. The returned cursor
    /// continues after the last item of the page.
    ///
    /// # Errors
    ///
    /// [`StoreError::InvalidCursor`] if `page.after` is not a cursor issued by
    /// this method; otherwise a backend error.
    pub async fn list_transactions(
        &self,
        filter: &TransactionFilter,
        page: &PageRequest,
    ) -> Result<Page<TransactionRecord>, StoreError> {
        let limit = page.effective_limit();
        let after = page.after.as_deref().map(parse_cursor).transpose()?;
        if matches!(&filter.scope, AccountScope::Only(ids) if ids.is_empty()) {
            return Ok(Page {
                items: Vec::new(),
                next: None,
            });
        }
        let mut query = QueryBuilder::<Sqlite>::new(concat!(
            "SELECT ",
            columns!(),
            " FROM transactions WHERE 1 = 1"
        ));
        push_filter(&mut query, filter);
        if let Some((day, id)) = after {
            query
                .push(" AND (posted_on < ")
                .push_bind(day.to_string())
                .push(" OR (posted_on = ")
                .push_bind(day.to_string())
                .push(" AND id < ")
                .push_bind(id.to_string())
                .push("))");
        }
        // One extra row tells whether another page exists.
        query
            .push(" ORDER BY posted_on DESC, id DESC LIMIT ")
            .push_bind(i64::from(limit) + 1);
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
        let next = if items.len() > limit as usize {
            items.truncate(limit as usize);
            items.last().map(cursor)
        } else {
            None
        };
        attach_children(&mut read, &mut items).await?;
        Ok(Page { items, next })
    }
}
