//! Category groups and categories: household-wide, ordered and archivable.

use coffer_domain::audit::AuditChanges;
use coffer_domain::identity::Actor;
use coffer_domain::ids::{CategoryGroupId, CategoryId};
use coffer_domain::policy::{Action, authorize};
use coffer_domain::transactions::{CategoryGroupName, CategoryKind, CategoryName, MAX_POSITION};
use coffer_store::{
    CategoryChanges, CategoryGroupChanges, CategoryGroupRecord, CategoryRecord, NewCategory,
    NewCategoryGroup, StoreError,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Ledger, LedgerError, audit, complete_created};
use crate::http::idempotency::IdempotencySlot;

const GROUP_NAME_INVALID: &str =
    "category group name must be 1 to 100 characters without control characters";
const NAME_INVALID: &str = "category name must be 1 to 100 characters without control characters";
const POSITION_INVALID: &str = "position must be at most 10000";
const UNKNOWN_GROUP: &str = "the category group does not exist";

/// A category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CategoryView {
    pub id: CategoryId,
    /// The group it belongs to.
    pub group: CategoryGroupId,
    pub name: String,
    pub kind: CategoryKind,
    /// Its place within its group: lower first, ties oldest first.
    pub position: u32,
    /// An archived category (or one in an archived group) takes no new
    /// splits; splits it already has stay.
    pub archived: bool,
}

impl CategoryView {
    fn new(record: &CategoryRecord) -> Self {
        Self {
            id: record.id,
            group: record.group,
            name: record.name.as_str().to_owned(),
            kind: record.kind,
            position: record.position,
            archived: record.archived_at.is_some(),
        }
    }
}

/// A category group with its categories in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CategoryGroupView {
    pub id: CategoryGroupId,
    pub name: String,
    /// Its place among groups: lower first, ties oldest first.
    pub position: u32,
    /// Every category of an archived group counts as archived.
    pub archived: bool,
    pub categories: Vec<CategoryView>,
}

impl CategoryGroupView {
    fn new(record: &CategoryGroupRecord) -> Self {
        Self {
            id: record.id,
            name: record.name.as_str().to_owned(),
            position: record.position,
            archived: record.archived_at.is_some(),
            categories: record.categories.iter().map(CategoryView::new).collect(),
        }
    }
}

/// Every category group with its categories, archived ones included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CategoryTree {
    pub items: Vec<CategoryGroupView>,
}

/// A category group to create.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CategoryGroupInput {
    /// 1 to 100 characters, without control characters.
    pub name: String,
    /// 0 to 10000; by default after the last group.
    #[schema(maximum = 10000)]
    pub position: Option<u32>,
}

/// Changes to a category group; absent (or `null`) fields stay as they
/// are.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CategoryGroupPatch {
    /// 1 to 100 characters, without control characters.
    pub name: Option<String>,
    /// 0 to 10000.
    #[schema(maximum = 10000)]
    pub position: Option<u32>,
    /// `true` archives the group, `false` restores it.
    pub archived: Option<bool>,
}

/// A category to create.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CategoryInput {
    /// The group to create it in.
    pub group: CategoryGroupId,
    /// 1 to 100 characters, without control characters.
    pub name: String,
    pub kind: CategoryKind,
    /// 0 to 10000; by default after the last category of the group.
    #[schema(maximum = 10000)]
    pub position: Option<u32>,
}

/// Changes to a category; absent (or `null`) fields stay as they are.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CategoryPatch {
    /// Moves the category to this group.
    pub group: Option<CategoryGroupId>,
    /// 1 to 100 characters, without control characters.
    pub name: Option<String>,
    pub kind: Option<CategoryKind>,
    /// 0 to 10000.
    #[schema(maximum = 10000)]
    pub position: Option<u32>,
    /// `true` archives the category, `false` restores it.
    pub archived: Option<bool>,
}

fn check_position(position: Option<u32>) -> Result<Option<u32>, LedgerError> {
    match position {
        Some(position) if position > MAX_POSITION => Err(LedgerError::Invalid(POSITION_INVALID)),
        other => Ok(other),
    }
}

fn group_name(name: &str) -> Result<CategoryGroupName, LedgerError> {
    CategoryGroupName::parse(name).map_err(|_| LedgerError::Invalid(GROUP_NAME_INVALID))
}

fn category_name(name: &str) -> Result<CategoryName, LedgerError> {
    CategoryName::parse(name).map_err(|_| LedgerError::Invalid(NAME_INVALID))
}

/// A group named in a request body that does not exist is an invalid
/// value, not a missing resource.
fn group_must_exist(err: StoreError) -> LedgerError {
    match err {
        StoreError::NotFound => LedgerError::Invalid(UNKNOWN_GROUP),
        other => other.into(),
    }
}

impl Ledger {
    /// Every category group with its categories.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] without [`Action::ReadHousehold`]; storage
    /// failures.
    pub async fn list_categories(&self, actor: &Actor) -> Result<CategoryTree, LedgerError> {
        authorize(actor, Action::ReadHousehold)?;
        let groups = self.store.list_categories().await?;
        Ok(CategoryTree {
            items: groups.iter().map(CategoryGroupView::new).collect(),
        })
    }

    /// Creates a category group. With `slot`, the idempotency key is
    /// completed with the response in the same transaction.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] without [`Action::WriteHousehold`];
    /// [`LedgerError::Invalid`] for an invalid name or position; storage
    /// failures.
    pub async fn create_category_group(
        &self,
        actor: &Actor,
        input: CategoryGroupInput,
        slot: Option<IdempotencySlot>,
    ) -> Result<CategoryGroupView, LedgerError> {
        self.authorize_category_write(actor).await?;
        let name = group_name(&input.name)?;
        let position = check_position(input.position)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let record = tx
            .insert_category_group(NewCategoryGroup {
                name,
                position,
                created_at: now,
            })
            .await?;
        let changes = AuditChanges::new()
            .set("name", record.name.as_str())
            .set("position", i64::from(record.position));
        audit(
            &mut tx,
            now,
            actor,
            "category_group.create",
            "category_group",
            record.id,
            changes,
        )
        .await?;
        let view = CategoryGroupView::new(&record);
        complete_created(&mut tx, slot, &view).await?;
        tx.commit().await?;
        Ok(view)
    }

    /// Changes a category group. A patch that changes nothing writes
    /// nothing.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown group;
    /// [`LedgerError::Denied`] without [`Action::WriteHousehold`];
    /// [`LedgerError::Invalid`] for an invalid name or position; storage
    /// failures.
    pub async fn update_category_group(
        &self,
        actor: &Actor,
        id: CategoryGroupId,
        patch: CategoryGroupPatch,
    ) -> Result<CategoryGroupView, LedgerError> {
        self.authorize_category_write(actor).await?;
        let name = patch.name.as_deref().map(group_name).transpose()?;
        let position = check_position(patch.position)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let current = tx.get_category_group(id).await?;

        let mut changes = CategoryGroupChanges::default();
        let mut audited = AuditChanges::new();
        if let Some(name) = name.filter(|name| *name != current.name) {
            audited = audited.change("name", current.name.as_str(), name.as_str());
            changes.name = Some(name);
        }
        if let Some(position) = position.filter(|position| *position != current.position) {
            audited = audited.change("position", i64::from(current.position), i64::from(position));
            changes.position = Some(position);
        }
        let was_archived = current.archived_at.is_some();
        if let Some(archived) = patch.archived.filter(|archived| *archived != was_archived) {
            audited = audited.change("archived", was_archived, archived);
            changes.archived_at = Some(archived.then_some(now));
        }
        if audited.is_empty() {
            return Ok(CategoryGroupView::new(&current));
        }
        let record = tx.update_category_group(id, changes).await?;
        audit(
            &mut tx,
            now,
            actor,
            "category_group.update",
            "category_group",
            id,
            audited,
        )
        .await?;
        tx.commit().await?;
        Ok(CategoryGroupView::new(&record))
    }

    /// Creates a category in an existing group. With `slot`, the
    /// idempotency key is completed with the response in the same
    /// transaction.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] without [`Action::WriteHousehold`];
    /// [`LedgerError::Invalid`] for an invalid name or position or an
    /// unknown group; storage failures.
    pub async fn create_category(
        &self,
        actor: &Actor,
        input: CategoryInput,
        slot: Option<IdempotencySlot>,
    ) -> Result<CategoryView, LedgerError> {
        self.authorize_category_write(actor).await?;
        let name = category_name(&input.name)?;
        let position = check_position(input.position)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        tx.get_category_group(input.group)
            .await
            .map_err(group_must_exist)?;
        let record = tx
            .insert_category(NewCategory {
                group: input.group,
                name,
                kind: input.kind,
                position,
                created_at: now,
            })
            .await?;
        let changes = AuditChanges::new()
            .set("group", record.group)
            .set("name", record.name.as_str())
            .set("kind", record.kind.as_str())
            .set("position", i64::from(record.position));
        audit(
            &mut tx,
            now,
            actor,
            "category.create",
            "category",
            record.id,
            changes,
        )
        .await?;
        let view = CategoryView::new(&record);
        complete_created(&mut tx, slot, &view).await?;
        tx.commit().await?;
        Ok(view)
    }

    /// Changes a category. A patch that changes nothing writes nothing.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown category;
    /// [`LedgerError::Denied`] without [`Action::WriteHousehold`];
    /// [`LedgerError::Invalid`] for an invalid name or position or an
    /// unknown group; storage failures.
    pub async fn update_category(
        &self,
        actor: &Actor,
        id: CategoryId,
        patch: CategoryPatch,
    ) -> Result<CategoryView, LedgerError> {
        self.authorize_category_write(actor).await?;
        let name = patch.name.as_deref().map(category_name).transpose()?;
        let position = check_position(patch.position)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let current = tx.get_category(id).await?;

        let mut changes = CategoryChanges::default();
        let mut audited = AuditChanges::new();
        if let Some(group) = patch.group.filter(|group| *group != current.group) {
            tx.get_category_group(group)
                .await
                .map_err(group_must_exist)?;
            audited = audited.change("group", current.group, group);
            changes.group = Some(group);
        }
        if let Some(name) = name.filter(|name| *name != current.name) {
            audited = audited.change("name", current.name.as_str(), name.as_str());
            changes.name = Some(name);
        }
        if let Some(kind) = patch.kind.filter(|kind| *kind != current.kind) {
            audited = audited.change("kind", current.kind.as_str(), kind.as_str());
            changes.kind = Some(kind);
        }
        if let Some(position) = position.filter(|position| *position != current.position) {
            audited = audited.change("position", i64::from(current.position), i64::from(position));
            changes.position = Some(position);
        }
        let was_archived = current.archived_at.is_some();
        if let Some(archived) = patch.archived.filter(|archived| *archived != was_archived) {
            audited = audited.change("archived", was_archived, archived);
            changes.archived_at = Some(archived.then_some(now));
        }
        if audited.is_empty() {
            return Ok(CategoryView::new(&current));
        }
        let record = tx.update_category(id, changes).await?;
        audit(
            &mut tx,
            now,
            actor,
            "category.update",
            "category",
            id,
            audited,
        )
        .await?;
        tx.commit().await?;
        Ok(CategoryView::new(&record))
    }
}
