//! Transaction source records: what providers report, each linked to the
//! canonical transaction it belongs to (or held while its provider account
//! is unbound), plus the matching queries and the imported-field, flag and
//! duplicate-candidate writes the sync makes on canonical transactions.

use std::collections::HashMap;

use coffer_domain::ids::{
    AccountId, ConnectionId, ProviderAccountId, SourceRecordId, TransactionId,
};
use coffer_domain::matching::{FINGERPRINT_VERSION, fingerprint};
use coffer_domain::transactions::{Description, Memo, Payee, TransactionFlag};
use jiff::Timestamp;
use jiff::civil::Date;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite, SqliteConnection};

use crate::convert::{corrupt, parsed, parsed_opt, text, text_opt, time};
use crate::members::expect_one;
use crate::transactions::flag_column;
use crate::{StoreError, StoreTx};

macro_rules! columns {
    () => {
        "id, connection_id, provider_account_ref, provider_txn_id, transaction_id, posted_on, \
         transacted_on, amount_raw, amount_minor, description, payee, memo, pending, \
         fingerprint, fingerprint_version, first_seen_at, last_seen_at"
    };
}

/// The fields a provider record sets, on its source record and on the
/// canonical transaction it belongs to. Texts are already within the
/// transaction limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedFields {
    pub posted_on: Date,
    pub transacted_on: Option<Date>,
    pub pending: bool,
    /// In the account currency's minor units; negative is money out.
    pub amount_minor: i64,
    pub description: Description,
    pub payee: Option<Payee>,
    pub memo: Option<Memo>,
}

/// A stored source record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRecord {
    pub id: SourceRecordId,
    pub connection: ConnectionId,
    pub provider_account: ProviderAccountId,
    /// The provider's id for the record, unique within its connection.
    pub provider_txn_id: String,
    /// The canonical transaction; `None` while held.
    pub transaction: Option<TransactionId>,
    pub fields: ImportedFields,
    /// The amount exactly as the provider wrote it, in its own sign (a
    /// decimal text).
    pub amount_raw: String,
    pub fingerprint: String,
    pub fingerprint_version: u8,
    pub first_seen_at: Timestamp,
    pub last_seen_at: Timestamp,
}

/// A source record to create. Its fingerprint is computed from its fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSource {
    pub connection: ConnectionId,
    pub provider_account: ProviderAccountId,
    pub provider_txn_id: String,
    /// `None` holds it until its provider account is bound.
    pub transaction: Option<TransactionId>,
    pub fields: ImportedFields,
    pub amount_raw: String,
    pub seen_at: Timestamp,
}

/// A source record's fields as the latest fetch reports them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceUpdate {
    pub fields: ImportedFields,
    pub amount_raw: String,
    pub seen_at: Timestamp,
}

/// An imported transaction that may receive an incoming record, with its
/// source records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateRow {
    pub transaction: TransactionId,
    pub posted_on: Date,
    pub amount_minor: i64,
    pub description: Description,
    pub sources: Vec<SourceRecord>,
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

fn source_from_row(row: &SqliteRow) -> Result<SourceRecord, StoreError> {
    let pending = match row
        .try_get::<i64, _>("pending")
        .map_err(StoreError::from_sqlx)?
    {
        0 => false,
        1 => true,
        _ => return Err(corrupt("pending is neither 0 nor 1")),
    };
    let version: i64 = row
        .try_get("fingerprint_version")
        .map_err(StoreError::from_sqlx)?;
    Ok(SourceRecord {
        id: parsed(row, "id")?,
        connection: parsed(row, "connection_id")?,
        provider_account: parsed(row, "provider_account_ref")?,
        provider_txn_id: text(row, "provider_txn_id")?,
        transaction: parsed_opt(row, "transaction_id")?,
        fields: ImportedFields {
            posted_on: parsed(row, "posted_on")?,
            transacted_on: parsed_opt(row, "transacted_on")?,
            pending,
            amount_minor: row.try_get("amount_minor").map_err(StoreError::from_sqlx)?,
            description: Description::parse(&text(row, "description")?)
                .map_err(StoreError::backend)?,
            payee: label(row, "payee", Payee::parse)?,
            memo: label(row, "memo", Memo::parse)?,
        },
        amount_raw: text(row, "amount_raw")?,
        fingerprint: text(row, "fingerprint")?,
        fingerprint_version: u8::try_from(version)
            .map_err(|_| corrupt("fingerprint version out of range"))?,
        first_seen_at: time(row, "first_seen_at")?,
        last_seen_at: time(row, "last_seen_at")?,
    })
}

async fn fetch_sources(
    conn: &mut SqliteConnection,
    condition: &str,
    bind: String,
) -> Result<Vec<SourceRecord>, StoreError> {
    let mut query = QueryBuilder::<Sqlite>::new(concat!(
        "SELECT ",
        columns!(),
        " FROM transaction_sources WHERE "
    ));
    query
        .push(condition)
        .push_bind(bind)
        .push(" ORDER BY posted_on, id");
    let rows = query
        .build()
        .fetch_all(&mut *conn)
        .await
        .map_err(StoreError::from_sqlx)?;
    rows.iter().map(source_from_row).collect()
}

/// The source records of each of `ids`, oldest posting day first.
pub(crate) async fn sources_by_transaction(
    conn: &mut SqliteConnection,
    ids: &[TransactionId],
) -> Result<HashMap<TransactionId, Vec<SourceRecord>>, StoreError> {
    let mut grouped: HashMap<TransactionId, Vec<SourceRecord>> = HashMap::new();
    if ids.is_empty() {
        return Ok(grouped);
    }
    let mut query = QueryBuilder::<Sqlite>::new(concat!(
        "SELECT ",
        columns!(),
        " FROM transaction_sources WHERE transaction_id IN ("
    ));
    let mut separated = query.separated(", ");
    for id in ids {
        separated.push_bind(id.to_string());
    }
    separated.push_unseparated(") ORDER BY posted_on, id");
    let rows = query
        .build()
        .fetch_all(&mut *conn)
        .await
        .map_err(StoreError::from_sqlx)?;
    for row in &rows {
        let source = source_from_row(row)?;
        let owner = source
            .transaction
            .ok_or_else(|| corrupt("a held source listed under a transaction"))?;
        grouped.entry(owner).or_default().push(source);
    }
    Ok(grouped)
}

/// The duplicate candidates of each of `ids`, in id order.
pub(crate) async fn candidates_by_transaction(
    conn: &mut SqliteConnection,
    ids: &[TransactionId],
) -> Result<HashMap<TransactionId, Vec<TransactionId>>, StoreError> {
    let mut grouped: HashMap<TransactionId, Vec<TransactionId>> = HashMap::new();
    if ids.is_empty() {
        return Ok(grouped);
    }
    let mut query = QueryBuilder::<Sqlite>::new(
        "SELECT transaction_id, candidate_id FROM duplicate_candidates WHERE transaction_id IN (",
    );
    let mut separated = query.separated(", ");
    for id in ids {
        separated.push_bind(id.to_string());
    }
    separated.push_unseparated(") ORDER BY transaction_id, candidate_id");
    let rows = query
        .build()
        .fetch_all(&mut *conn)
        .await
        .map_err(StoreError::from_sqlx)?;
    for row in &rows {
        grouped
            .entry(parsed(row, "transaction_id")?)
            .or_default()
            .push(parsed(row, "candidate_id")?);
    }
    Ok(grouped)
}

impl StoreTx {
    /// The source record `provider_txn_id` of `connection`, if any.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn find_source(
        &mut self,
        connection: ConnectionId,
        provider_txn_id: &str,
    ) -> Result<Option<SourceRecord>, StoreError> {
        let row = sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM transaction_sources WHERE connection_id = ? AND provider_txn_id = ?"
        ))
        .bind(connection.to_string())
        .bind(provider_txn_id)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        row.as_ref().map(source_from_row).transpose()
    }

    /// Create a source record, fingerprinted with the current
    /// [`FINGERPRINT_VERSION`].
    ///
    /// # Errors
    ///
    /// [`StoreError::Conflict`] if its connection already has a record with
    /// this provider id; backend failures.
    pub async fn insert_source(&mut self, new: NewSource) -> Result<SourceRecord, StoreError> {
        let id = SourceRecordId::new();
        let seen = new.seen_at.as_millisecond();
        let fields = &new.fields;
        sqlx::query(
            "INSERT INTO transaction_sources (id, connection_id, provider_account_ref, \
             provider_txn_id, transaction_id, posted_on, transacted_on, amount_raw, \
             amount_minor, description, payee, memo, pending, fingerprint, \
             fingerprint_version, first_seen_at, last_seen_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(new.connection.to_string())
        .bind(new.provider_account.to_string())
        .bind(&new.provider_txn_id)
        .bind(new.transaction.map(|t| t.to_string()))
        .bind(fields.posted_on.to_string())
        .bind(fields.transacted_on.map(|day| day.to_string()))
        .bind(&new.amount_raw)
        .bind(fields.amount_minor)
        .bind(fields.description.as_str())
        .bind(fields.payee.as_ref().map(Payee::as_str))
        .bind(fields.memo.as_ref().map(Memo::as_str))
        .bind(fields.pending)
        .bind(fingerprint(
            fields.amount_minor,
            fields.description.as_str(),
        ))
        .bind(i64::from(FINGERPRINT_VERSION))
        .bind(seen)
        .bind(seen)
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        self.get_source(id).await
    }

    /// Fetch a source record.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such record.
    pub async fn get_source(&mut self, id: SourceRecordId) -> Result<SourceRecord, StoreError> {
        let row = sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM transaction_sources WHERE id = ?"
        ))
        .bind(id.to_string())
        .fetch_one(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        source_from_row(&row)
    }

    /// Replace a source record's fields with what the latest fetch reports,
    /// refingerprinted with the current [`FINGERPRINT_VERSION`].
    ///
    /// # Errors
    ///
    /// `NotFound` if the record does not exist.
    pub async fn update_source(
        &mut self,
        id: SourceRecordId,
        update: SourceUpdate,
    ) -> Result<(), StoreError> {
        let fields = &update.fields;
        let result = sqlx::query(
            "UPDATE transaction_sources SET posted_on = ?, transacted_on = ?, amount_raw = ?, \
             amount_minor = ?, description = ?, payee = ?, memo = ?, pending = ?, \
             fingerprint = ?, fingerprint_version = ?, last_seen_at = ? WHERE id = ?",
        )
        .bind(fields.posted_on.to_string())
        .bind(fields.transacted_on.map(|day| day.to_string()))
        .bind(&update.amount_raw)
        .bind(fields.amount_minor)
        .bind(fields.description.as_str())
        .bind(fields.payee.as_ref().map(Payee::as_str))
        .bind(fields.memo.as_ref().map(Memo::as_str))
        .bind(fields.pending)
        .bind(fingerprint(
            fields.amount_minor,
            fields.description.as_str(),
        ))
        .bind(i64::from(FINGERPRINT_VERSION))
        .bind(update.seen_at.as_millisecond())
        .bind(id.to_string())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// Link a source record to `transaction`.
    ///
    /// # Errors
    ///
    /// `NotFound` if the record does not exist; backend failures, including
    /// a transaction that does not exist.
    pub async fn attach_source(
        &mut self,
        id: SourceRecordId,
        transaction: TransactionId,
    ) -> Result<(), StoreError> {
        let result = sqlx::query("UPDATE transaction_sources SET transaction_id = ? WHERE id = ?")
            .bind(transaction.to_string())
            .bind(id.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// Delete a source record, detaching it from its transaction.
    ///
    /// # Errors
    ///
    /// `NotFound` if the record does not exist.
    pub async fn delete_source(&mut self, id: SourceRecordId) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM transaction_sources WHERE id = ?")
            .bind(id.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// The source records of `transaction`, oldest posting day first.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn sources_of(
        &mut self,
        transaction: TransactionId,
    ) -> Result<Vec<SourceRecord>, StoreError> {
        fetch_sources(&mut self.tx, "transaction_id = ", transaction.to_string()).await
    }

    /// The pending source records of `provider_account` posted on or after
    /// `since`, oldest first.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn pending_sources_since(
        &mut self,
        provider_account: ProviderAccountId,
        since: Date,
    ) -> Result<Vec<SourceRecord>, StoreError> {
        let rows = sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM transaction_sources WHERE provider_account_ref = ? AND pending = 1 \
             AND posted_on >= ? ORDER BY posted_on, id"
        ))
        .bind(provider_account.to_string())
        .bind(since.to_string())
        .fetch_all(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        rows.iter().map(source_from_row).collect()
    }

    /// The held source records of `provider_account` (those without a
    /// transaction), oldest first.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn held_sources(
        &mut self,
        provider_account: ProviderAccountId,
    ) -> Result<Vec<SourceRecord>, StoreError> {
        fetch_sources(
            &mut self.tx,
            "transaction_id IS NULL AND provider_account_ref = ",
            provider_account.to_string(),
        )
        .await
    }

    /// The imported transactions of `account` posted from `from` to `to`
    /// (both included), each with its source records. Manual transactions
    /// are never candidates.
    ///
    /// # Errors
    ///
    /// Backend failures, or a row that no longer parses.
    pub async fn match_candidates(
        &mut self,
        account: AccountId,
        from: Date,
        to: Date,
    ) -> Result<Vec<CandidateRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, posted_on, amount_minor, raw_description FROM transactions \
             WHERE account_id = ? AND origin = 'imported' AND posted_on >= ? AND posted_on <= ? \
             ORDER BY posted_on, id",
        )
        .bind(account.to_string())
        .bind(from.to_string())
        .bind(to.to_string())
        .fetch_all(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        let mut candidates = rows
            .iter()
            .map(|row| {
                Ok(CandidateRow {
                    transaction: parsed(row, "id")?,
                    posted_on: parsed(row, "posted_on")?,
                    amount_minor: row.try_get("amount_minor").map_err(StoreError::from_sqlx)?,
                    description: Description::parse(&text(row, "raw_description")?)
                        .map_err(StoreError::backend)?,
                    sources: Vec::new(),
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        let ids: Vec<_> = candidates.iter().map(|c| c.transaction).collect();
        let mut sources = sources_by_transaction(&mut self.tx, &ids).await?;
        for candidate in &mut candidates {
            candidate.sources = sources.remove(&candidate.transaction).unwrap_or_default();
        }
        Ok(candidates)
    }

    /// Replace a transaction's imported fields. The payee members see
    /// follows the imported one unless a member edited it; no other user
    /// field changes.
    ///
    /// # Errors
    ///
    /// `NotFound` if the transaction does not exist.
    pub async fn update_imported_fields(
        &mut self,
        id: TransactionId,
        fields: &ImportedFields,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        let payee = fields.payee.as_ref().map(Payee::as_str);
        let result = sqlx::query(
            "UPDATE transactions SET posted_on = ?, transacted_on = ?, pending = ?, \
             amount_minor = ?, raw_description = ?, imported_payee = ?, \
             payee = CASE WHEN payee_edited = 1 THEN payee ELSE ? END, memo = ?, \
             updated_at = ? WHERE id = ?",
        )
        .bind(fields.posted_on.to_string())
        .bind(fields.transacted_on.map(|day| day.to_string()))
        .bind(fields.pending)
        .bind(fields.amount_minor)
        .bind(fields.description.as_str())
        .bind(payee)
        .bind(payee)
        .bind(fields.memo.as_ref().map(Memo::as_str))
        .bind(at.as_millisecond())
        .bind(id.to_string())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// Set or clear one of a transaction's flags.
    ///
    /// # Errors
    ///
    /// `NotFound` if the transaction does not exist.
    pub async fn set_flag(
        &mut self,
        id: TransactionId,
        flag: TransactionFlag,
        set: bool,
    ) -> Result<(), StoreError> {
        let mut query = QueryBuilder::<Sqlite>::new("UPDATE transactions SET ");
        query
            .push(flag_column(flag))
            .push(" = ")
            .push_bind(set)
            .push(" WHERE id = ")
            .push_bind(id.to_string());
        let result = query
            .build()
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// Record `candidates` as transactions `transaction` may duplicate;
    /// ones already recorded are kept.
    ///
    /// # Errors
    ///
    /// Backend failures, including a transaction that does not exist.
    pub async fn add_duplicate_candidates(
        &mut self,
        transaction: TransactionId,
        candidates: &[TransactionId],
    ) -> Result<(), StoreError> {
        for candidate in candidates {
            sqlx::query(
                "INSERT INTO duplicate_candidates (transaction_id, candidate_id) VALUES (?, ?) \
                 ON CONFLICT DO NOTHING",
            )
            .bind(transaction.to_string())
            .bind(candidate.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        }
        Ok(())
    }

    /// The transactions that list `candidate` as one they may duplicate,
    /// in id order.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn listed_as_candidate(
        &mut self,
        candidate: TransactionId,
    ) -> Result<Vec<TransactionId>, StoreError> {
        let rows = sqlx::query(
            "SELECT transaction_id FROM duplicate_candidates WHERE candidate_id = ? \
             ORDER BY transaction_id",
        )
        .bind(candidate.to_string())
        .fetch_all(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        rows.iter()
            .map(|row| parsed(row, "transaction_id"))
            .collect()
    }

    /// Forget `candidate` as one `transaction` may duplicate. Forgetting a
    /// pair that is not recorded does nothing.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn remove_duplicate_candidate(
        &mut self,
        transaction: TransactionId,
        candidate: TransactionId,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "DELETE FROM duplicate_candidates WHERE transaction_id = ? AND candidate_id = ?",
        )
        .bind(transaction.to_string())
        .bind(candidate.to_string())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(())
    }

    /// Forget every duplicate candidate of `transaction`.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn clear_duplicate_candidates(
        &mut self,
        transaction: TransactionId,
    ) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM duplicate_candidates WHERE transaction_id = ?")
            .bind(transaction.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        Ok(())
    }
}
