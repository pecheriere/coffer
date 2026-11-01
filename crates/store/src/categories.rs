//! Category groups and their categories.

use coffer_domain::ids::{CategoryGroupId, CategoryId};
use coffer_domain::transactions::{CategoryGroupName, CategoryKind, CategoryName, MAX_POSITION};
use jiff::Timestamp;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite, SqliteExecutor};

use crate::convert::{corrupt, parsed, text, time, time_opt};
use crate::members::expect_one;
use crate::{Store, StoreError, StoreTx};

macro_rules! group_columns {
    () => {
        "id, name, position, archived_at, created_at"
    };
}

macro_rules! category_columns {
    () => {
        "id, group_id, name, kind, position, archived_at, created_at"
    };
}

/// A stored category group with its categories, in position order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryGroupRecord {
    pub id: CategoryGroupId,
    pub name: CategoryGroupName,
    pub position: u32,
    /// When the group was archived; `None` while in use.
    pub archived_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub categories: Vec<CategoryRecord>,
}

/// A stored category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryRecord {
    pub id: CategoryId,
    pub group: CategoryGroupId,
    pub name: CategoryName,
    pub kind: CategoryKind,
    pub position: u32,
    /// When the category was archived; `None` while in use.
    pub archived_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

/// A category group to create.
#[derive(Debug, Clone)]
pub struct NewCategoryGroup {
    pub name: CategoryGroupName,
    /// `None` places the group after the last one.
    pub position: Option<u32>,
    pub created_at: Timestamp,
}

/// A category to create in an existing group.
#[derive(Debug, Clone)]
pub struct NewCategory {
    pub group: CategoryGroupId,
    pub name: CategoryName,
    pub kind: CategoryKind,
    /// `None` places the category after the last one of its group.
    pub position: Option<u32>,
    pub created_at: Timestamp,
}

/// The fields of a category group to change; `None` leaves a field as it
/// is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CategoryGroupChanges {
    pub name: Option<CategoryGroupName>,
    pub position: Option<u32>,
    /// `Some(None)` unarchives the group.
    pub archived_at: Option<Option<Timestamp>>,
}

/// The fields of a category to change; `None` leaves a field as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CategoryChanges {
    /// Moves the category to another group.
    pub group: Option<CategoryGroupId>,
    pub name: Option<CategoryName>,
    pub kind: Option<CategoryKind>,
    pub position: Option<u32>,
    /// `Some(None)` unarchives the category.
    pub archived_at: Option<Option<Timestamp>>,
}

fn position(row: &SqliteRow) -> Result<u32, StoreError> {
    let stored: i64 = row.try_get("position").map_err(StoreError::from_sqlx)?;
    u32::try_from(stored)
        .ok()
        .filter(|position| *position <= MAX_POSITION)
        .ok_or_else(|| corrupt("position out of range"))
}

fn group_from_row(row: &SqliteRow) -> Result<CategoryGroupRecord, StoreError> {
    Ok(CategoryGroupRecord {
        id: parsed(row, "id")?,
        name: CategoryGroupName::parse(&text(row, "name")?).map_err(StoreError::backend)?,
        position: position(row)?,
        archived_at: time_opt(row, "archived_at")?,
        created_at: time(row, "created_at")?,
        categories: Vec::new(),
    })
}

fn category_from_row(row: &SqliteRow) -> Result<CategoryRecord, StoreError> {
    Ok(CategoryRecord {
        id: parsed(row, "id")?,
        group: parsed(row, "group_id")?,
        name: CategoryName::parse(&text(row, "name")?).map_err(StoreError::backend)?,
        kind: parsed(row, "kind")?,
        position: position(row)?,
        archived_at: time_opt(row, "archived_at")?,
        created_at: time(row, "created_at")?,
    })
}

/// The position after the last row of `table` matching `group` (every row
/// when `None`), capped at [`MAX_POSITION`].
async fn next_position<'e>(
    executor: impl SqliteExecutor<'e>,
    table: &'static str,
    group: Option<CategoryGroupId>,
) -> Result<u32, StoreError> {
    let mut query = QueryBuilder::<Sqlite>::new("SELECT max(position) AS last FROM ");
    query.push(table);
    if let Some(group) = group {
        query
            .push(" WHERE group_id = ")
            .push_bind(group.to_string());
    }
    let row = query
        .build()
        .fetch_one(executor)
        .await
        .map_err(StoreError::from_sqlx)?;
    let last: Option<i64> = row.try_get("last").map_err(StoreError::from_sqlx)?;
    let next = last.map_or(0, |last| last.saturating_add(1));
    Ok(u32::try_from(next.min(i64::from(MAX_POSITION))).unwrap_or(MAX_POSITION))
}

async fn fetch_category<'e>(
    executor: impl SqliteExecutor<'e>,
    id: CategoryId,
) -> Result<CategoryRecord, StoreError> {
    let row = sqlx::query(concat!(
        "SELECT ",
        category_columns!(),
        " FROM categories WHERE id = ?"
    ))
    .bind(id.to_string())
    .fetch_one(executor)
    .await
    .map_err(StoreError::from_sqlx)?;
    category_from_row(&row)
}

async fn fetch_group(
    tx: &mut StoreTx,
    id: CategoryGroupId,
) -> Result<CategoryGroupRecord, StoreError> {
    let row = sqlx::query(concat!(
        "SELECT ",
        group_columns!(),
        " FROM category_groups WHERE id = ?"
    ))
    .bind(id.to_string())
    .fetch_one(&mut *tx.tx)
    .await
    .map_err(StoreError::from_sqlx)?;
    let mut group = group_from_row(&row)?;
    let rows = sqlx::query(concat!(
        "SELECT ",
        category_columns!(),
        " FROM categories WHERE group_id = ? ORDER BY position, id"
    ))
    .bind(id.to_string())
    .fetch_all(&mut *tx.tx)
    .await
    .map_err(StoreError::from_sqlx)?;
    group.categories = rows
        .iter()
        .map(category_from_row)
        .collect::<Result<_, _>>()?;
    Ok(group)
}

impl StoreTx {
    /// Create a category group.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn insert_category_group(
        &mut self,
        new: NewCategoryGroup,
    ) -> Result<CategoryGroupRecord, StoreError> {
        let position = match new.position {
            Some(position) => position,
            None => next_position(&mut *self.tx, "category_groups", None).await?,
        };
        let id = CategoryGroupId::new();
        sqlx::query(
            "INSERT INTO category_groups (id, name, position, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(new.name.as_str())
        .bind(i64::from(position))
        .bind(new.created_at.as_millisecond())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(CategoryGroupRecord {
            id,
            name: new.name,
            position,
            archived_at: None,
            created_at: new.created_at,
            categories: Vec::new(),
        })
    }

    /// Apply `changes` to a category group and return it, with its
    /// categories, as it now is.
    ///
    /// # Errors
    ///
    /// `NotFound` if the group does not exist.
    pub async fn update_category_group(
        &mut self,
        id: CategoryGroupId,
        changes: CategoryGroupChanges,
    ) -> Result<CategoryGroupRecord, StoreError> {
        let mut query = QueryBuilder::<Sqlite>::new("UPDATE category_groups SET id = id");
        if let Some(name) = &changes.name {
            query.push(", name = ").push_bind(name.as_str());
        }
        if let Some(position) = changes.position {
            query.push(", position = ").push_bind(i64::from(position));
        }
        if let Some(archived_at) = changes.archived_at {
            query
                .push(", archived_at = ")
                .push_bind(archived_at.map(Timestamp::as_millisecond));
        }
        query.push(" WHERE id = ").push_bind(id.to_string());
        let result = query
            .build()
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())?;
        fetch_group(self, id).await
    }

    /// Fetch a category group with its categories, as seen by this
    /// transaction.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such group.
    pub async fn get_category_group(
        &mut self,
        id: CategoryGroupId,
    ) -> Result<CategoryGroupRecord, StoreError> {
        fetch_group(self, id).await
    }

    /// Create a category in an existing group.
    ///
    /// # Errors
    ///
    /// Backend failures, including a group that does not exist.
    pub async fn insert_category(
        &mut self,
        new: NewCategory,
    ) -> Result<CategoryRecord, StoreError> {
        let position = match new.position {
            Some(position) => position,
            None => next_position(&mut *self.tx, "categories", Some(new.group)).await?,
        };
        let id = CategoryId::new();
        sqlx::query(
            "INSERT INTO categories (id, group_id, name, kind, position, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(new.group.to_string())
        .bind(new.name.as_str())
        .bind(new.kind.as_str())
        .bind(i64::from(position))
        .bind(new.created_at.as_millisecond())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(CategoryRecord {
            id,
            group: new.group,
            name: new.name,
            kind: new.kind,
            position,
            archived_at: None,
            created_at: new.created_at,
        })
    }

    /// Apply `changes` to a category and return it as it now is.
    ///
    /// # Errors
    ///
    /// `NotFound` if the category does not exist; backend failures,
    /// including a target group that does not exist.
    pub async fn update_category(
        &mut self,
        id: CategoryId,
        changes: CategoryChanges,
    ) -> Result<CategoryRecord, StoreError> {
        let mut query = QueryBuilder::<Sqlite>::new("UPDATE categories SET id = id");
        if let Some(group) = changes.group {
            query.push(", group_id = ").push_bind(group.to_string());
        }
        if let Some(name) = &changes.name {
            query.push(", name = ").push_bind(name.as_str());
        }
        if let Some(kind) = changes.kind {
            query.push(", kind = ").push_bind(kind.as_str());
        }
        if let Some(position) = changes.position {
            query.push(", position = ").push_bind(i64::from(position));
        }
        if let Some(archived_at) = changes.archived_at {
            query
                .push(", archived_at = ")
                .push_bind(archived_at.map(Timestamp::as_millisecond));
        }
        query.push(" WHERE id = ").push_bind(id.to_string());
        let result = query
            .build()
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())?;
        fetch_category(&mut *self.tx, id).await
    }

    /// Fetch a category, as seen by this transaction.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such category.
    pub async fn get_category(&mut self, id: CategoryId) -> Result<CategoryRecord, StoreError> {
        fetch_category(&mut *self.tx, id).await
    }
}

impl Store {
    /// Every category group with its categories, each ordered by position
    /// then age; archived ones included.
    ///
    /// # Errors
    ///
    /// Backend failures, or a row that no longer parses.
    pub async fn list_categories(&self) -> Result<Vec<CategoryGroupRecord>, StoreError> {
        // One read transaction, so every category's group is in the list.
        let mut conn = self.pool.begin().await.map_err(StoreError::from_sqlx)?;
        let group_rows = sqlx::query(concat!(
            "SELECT ",
            group_columns!(),
            " FROM category_groups ORDER BY position, id"
        ))
        .fetch_all(&mut *conn)
        .await
        .map_err(StoreError::from_sqlx)?;
        let category_rows = sqlx::query(concat!(
            "SELECT ",
            category_columns!(),
            " FROM categories ORDER BY position, id"
        ))
        .fetch_all(&mut *conn)
        .await
        .map_err(StoreError::from_sqlx)?;
        let mut groups = group_rows
            .iter()
            .map(group_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        for row in &category_rows {
            let category = category_from_row(row)?;
            let group = groups
                .iter_mut()
                .find(|group| group.id == category.group)
                .ok_or_else(|| corrupt("category without its group"))?;
            group.categories.push(category);
        }
        Ok(groups)
    }
}
