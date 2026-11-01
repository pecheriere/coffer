//! The appearance service: the household theme and each member's theme and
//! colour scheme, resolved into the [`Look`] a page is drawn with. Every
//! change and its audit entry share one [`StoreTx`](coffer_store::StoreTx).

use std::sync::Arc;

use coffer_domain::audit::AuditChanges;
use coffer_domain::identity::{Actor, ColorScheme, IdentityError, ThemeName};
use coffer_domain::policy::{Action, authorize};
use coffer_domain::time::Clock;
use coffer_store::{Member, Store};

use crate::auth::{AuthError, audit};
use crate::themes::{self, DEFAULT, Look, Theme};

/// Entity id of the household's audit entries: there is one household.
const HOUSEHOLD: &str = "household";

/// The appearance service.
pub struct Appearance {
    store: Store,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Appearance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Appearance").finish_non_exhaustive()
    }
}

impl Appearance {
    #[must_use]
    pub fn new(store: Store, clock: Arc<dyn Clock>) -> Self {
        Self { store, clock }
    }

    /// How a page for `member` (or for nobody signed in) is drawn: the
    /// member's theme if it is registered, else the household theme if it
    /// is registered, else [`DEFAULT`]; the member's colour scheme, or
    /// [`ColorScheme::System`] without a member. Never fails: a storage
    /// error is logged and resolution falls back.
    pub async fn look(&self, member: Option<&Member>) -> Look {
        let own = member
            .and_then(|member| member.theme.as_ref())
            .and_then(|name| themes::find(name.as_str()));
        let theme = match own {
            Some(theme) => theme,
            None => self.household_theme().await,
        };
        Look {
            theme,
            scheme: member.map_or(ColorScheme::System, |member| member.color_scheme),
        }
    }

    /// The household theme if one is chosen and registered, else
    /// [`DEFAULT`]. Never fails: a storage error is logged and resolution
    /// falls back.
    pub async fn household_theme(&self) -> &'static Theme {
        match self.store.household_theme().await {
            Ok(name) => name
                .and_then(|name| themes::find(name.as_str()))
                .unwrap_or(DEFAULT),
            Err(err) => {
                tracing::warn!(error = %err, "household theme unreadable; using the default");
                DEFAULT
            }
        }
    }

    /// Sets the acting member's own theme (`None` follows the household
    /// theme) and colour scheme. Records only the fields that changed; a
    /// choice equal to the stored one writes nothing.
    ///
    /// # Errors
    ///
    /// [`AuthError::Denied`] without [`Action::ManageOwnPreferences`];
    /// [`AuthError::Invalid`] with [`IdentityError::UnknownTheme`] if `theme`
    /// is not registered; [`AuthError::NotFound`]; storage failures.
    pub async fn set_member(
        &self,
        actor: &Actor,
        theme: Option<ThemeName>,
        scheme: ColorScheme,
    ) -> Result<(), AuthError> {
        authorize(actor, Action::ManageOwnPreferences)?;
        if let Some(name) = &theme {
            registered(name)?;
        }
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let current = tx.get_member(actor.member).await?;
        let mut changes = AuditChanges::new();
        if current.theme != theme {
            changes = changes.change(
                "theme",
                current.theme.as_ref().map(ThemeName::as_str),
                theme.as_ref().map(ThemeName::as_str),
            );
        }
        if current.color_scheme != scheme {
            changes = changes.change(
                "color_scheme",
                current.color_scheme.as_str(),
                scheme.as_str(),
            );
        }
        if changes.is_empty() {
            return Ok(());
        }
        tx.set_member_appearance(actor.member, theme.as_ref(), scheme)
            .await?;
        audit(
            &mut tx,
            now,
            &actor.into(),
            "member.appearance",
            "member",
            actor.member,
            changes,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Sets the household theme. Setting the stored theme again writes
    /// nothing.
    ///
    /// # Errors
    ///
    /// [`AuthError::Denied`] without [`Action::ManageHousehold`];
    /// [`AuthError::Invalid`] with [`IdentityError::UnknownTheme`] if `theme`
    /// is not registered; storage failures.
    pub async fn set_household_theme(
        &self,
        actor: &Actor,
        theme: ThemeName,
    ) -> Result<(), AuthError> {
        authorize(actor, Action::ManageHousehold)?;
        registered(&theme)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let current = tx.household_theme().await?;
        if current.as_ref() == Some(&theme) {
            return Ok(());
        }
        tx.set_household_theme(&theme).await?;
        audit(
            &mut tx,
            now,
            &actor.into(),
            "household.theme",
            "household",
            HOUSEHOLD,
            AuditChanges::new().change(
                "theme",
                current.as_ref().map(ThemeName::as_str),
                theme.as_str(),
            ),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

fn registered(name: &ThemeName) -> Result<(), AuthError> {
    match themes::find(name.as_str()) {
        Some(_) => Ok(()),
        None => Err(AuthError::Invalid(IdentityError::UnknownTheme)),
    }
}
