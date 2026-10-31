//! The identity service: bootstrap, invites, enrollment, re-enroll, login and
//! member management. Every mutation and its audit entry share one
//! [`StoreTx`].

use std::sync::Arc;

use coffer_domain::audit::AuditActor;
use coffer_domain::identity::{
    Actor, DisplayName, IdentityError, Role, Scope, TokenName, Username,
};
use coffer_domain::ids::{InviteId, MemberId, PasskeyId};
use coffer_domain::policy::{Action, Denied, DenyReason, authorize};
use coffer_domain::time::Clock;
use coffer_store::{
    Invite, InviteKind, NewInvite, NewMember, NewPasskey, PasskeyRecord, Store, StoreError, StoreTx,
};
use jiff::Timestamp;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use url::Url;
use webauthn_rs::prelude::{
    CreationChallengeResponse, CredentialID, Passkey, PublicKeyCredential,
    RegisterPublicKeyCredential, RequestChallengeResponse, WebauthnError,
};

use super::passkeys::{Ceremony, CeremonyId, EnrollTarget, Passkeys};
use super::{AuthError, audit};
use crate::config::Config;
use crate::secrets::{SecretToken, ServerKey, TokenKind, mint};

const BOOTSTRAP_REASON: &str = "system:bootstrap";
const CLI_REASON: &str = "system:cli";

/// Path prefix of the first-admin setup link.
const SETUP_PATH: &str = "setup";
/// Path prefix of new-member and re-enroll links.
const INVITE_PATH: &str = "invite";

/// What redeeming a link would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvitePreview {
    /// Creates a member; the invitee picks a username and may override the
    /// suggested display name.
    NewMember { display_name: Option<DisplayName> },
    /// Adds a passkey to an existing member.
    Reenroll,
}

/// The identity service.
pub struct Identity {
    store: Store,
    key: Arc<ServerKey>,
    passkeys: Passkeys,
    clock: Arc<dyn Clock>,
    config: Config,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity").finish_non_exhaustive()
    }
}

impl Identity {
    /// The service for `config.public_url`.
    ///
    /// # Errors
    ///
    /// [`AuthError::Webauthn`] if the public URL cannot be a `WebAuthn`
    /// relying party (see [`Passkeys::new`]).
    pub fn new(
        store: Store,
        key: Arc<ServerKey>,
        clock: Arc<dyn Clock>,
        config: Config,
    ) -> Result<Self, AuthError> {
        let passkeys = Passkeys::new(&config.public_url)?;
        Ok(Self {
            store,
            key,
            passkeys,
            clock,
            config,
        })
    }

    /// A fresh first-admin setup link, or `None` once an admin exists. Any
    /// earlier unused setup link is revoked, so only the newest one works.
    ///
    /// # Errors
    ///
    /// Storage failures.
    pub async fn bootstrap_link(&self) -> Result<Option<Url>, AuthError> {
        let now = self.clock.now();
        let actor = AuditActor::System {
            reason: BOOTSTRAP_REASON.to_owned(),
        };
        let mut tx = self.store.begin().await?;
        if admin_exists(&mut tx).await? {
            return Ok(None);
        }
        for invite in tx.list_pending_invites(now).await? {
            if invite.kind == InviteKind::Bootstrap {
                tx.revoke_invite(invite.id, now).await?;
                audit(
                    &mut tx,
                    now,
                    &actor,
                    "invite.revoke",
                    "invite",
                    invite.id,
                    json!({}),
                )
                .await?;
            }
        }
        let link = self
            .issue_invite(
                &mut tx,
                now,
                InviteKind::Bootstrap,
                None,
                &actor,
                SETUP_PATH,
            )
            .await?;
        tx.commit().await?;
        Ok(Some(link))
    }

    /// An invite link for a new member with `role`; `display_name` is a
    /// suggestion the invitee may override.
    ///
    /// # Errors
    ///
    /// [`AuthError::Denied`] without [`Action::IssueEnrollmentLink`] (agents
    /// never have it); storage failures.
    pub async fn create_invite(
        &self,
        actor: &Actor,
        role: Role,
        display_name: Option<DisplayName>,
    ) -> Result<Url, AuthError> {
        authorize(actor, Action::IssueEnrollmentLink)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let kind = InviteKind::NewMember { role, display_name };
        let link = self
            .issue_invite(
                &mut tx,
                now,
                kind,
                Some(actor.member),
                &actor.into(),
                INVITE_PATH,
            )
            .await?;
        tx.commit().await?;
        Ok(link)
    }

    /// A re-enroll link that adds a passkey to `member`.
    ///
    /// # Errors
    ///
    /// [`AuthError::Denied`] without [`Action::IssueEnrollmentLink`] (agents
    /// never have it); [`AuthError::NotFound`]; [`AuthError::MemberDisabled`];
    /// storage failures.
    pub async fn create_reenroll(&self, actor: &Actor, member: MemberId) -> Result<Url, AuthError> {
        authorize(actor, Action::IssueEnrollmentLink)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let target = tx.get_member(member).await?;
        if target.disabled_at.is_some() {
            return Err(AuthError::MemberDisabled);
        }
        let link = self
            .issue_invite(
                &mut tx,
                now,
                InviteKind::Reenroll { member },
                Some(actor.member),
                &actor.into(),
                INVITE_PATH,
            )
            .await?;
        tx.commit().await?;
        Ok(link)
    }

    /// A re-enroll link for admin `username`, for the host operator who has
    /// lost every admin passkey. Host access is the root of trust, so there
    /// is no actor; only admins can be recovered this way.
    ///
    /// # Errors
    ///
    /// [`AuthError::NotFound`]; [`AuthError::Denied`] if the member is not an
    /// admin; [`AuthError::MemberDisabled`]; storage failures.
    pub async fn cli_admin_reenroll(&self, username: &Username) -> Result<Url, AuthError> {
        let now = self.clock.now();
        let member = self.store.get_member_by_username(username).await?;
        let mut tx = self.store.begin().await?;
        let target = tx.get_member(member.id).await?;
        if target.role != Role::Admin {
            return Err(AuthError::Denied(Denied {
                action: Action::ManageMembers,
                reason: DenyReason::InsufficientScope {
                    needed: Scope::Admin,
                },
            }));
        }
        if target.disabled_at.is_some() {
            return Err(AuthError::MemberDisabled);
        }
        let actor = AuditActor::System {
            reason: CLI_REASON.to_owned(),
        };
        let link = self
            .issue_invite(
                &mut tx,
                now,
                InviteKind::Reenroll { member: target.id },
                None,
                &actor,
                INVITE_PATH,
            )
            .await?;
        tx.commit().await?;
        Ok(link)
    }

    /// Revokes a pending invite.
    ///
    /// # Errors
    ///
    /// [`AuthError::Denied`] without [`Action::ManageInvites`];
    /// [`AuthError::NotFound`] unless the invite is pending; storage failures.
    pub async fn revoke_invite(&self, actor: &Actor, invite: InviteId) -> Result<(), AuthError> {
        authorize(actor, Action::ManageInvites)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let pending = tx.list_pending_invites(now).await?;
        if !pending.iter().any(|candidate| candidate.id == invite) {
            return Err(AuthError::NotFound);
        }
        tx.revoke_invite(invite, now).await?;
        audit(
            &mut tx,
            now,
            &actor.into(),
            "invite.revoke",
            "invite",
            invite,
            json!({}),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Invites that are unused, unrevoked and unexpired.
    ///
    /// # Errors
    ///
    /// [`AuthError::Denied`] without [`Action::ManageInvites`]; storage
    /// failures.
    pub async fn list_pending_invites(&self, actor: &Actor) -> Result<Vec<Invite>, AuthError> {
        authorize(actor, Action::ManageInvites)?;
        Ok(self.store.list_pending_invites(self.clock.now()).await?)
    }

    /// What the link behind `invite_token` would do, so its page can ask for
    /// the right fields. Reveals nothing beyond what redeeming the link
    /// would.
    ///
    /// # Errors
    ///
    /// [`AuthError::InviteInvalid`] for an unknown, revoked, used or expired
    /// link, or a setup link once an admin exists; storage failures.
    pub async fn describe_invite(&self, invite_token: &str) -> Result<InvitePreview, AuthError> {
        let now = self.clock.now();
        let token_hash = self.key.hash(TokenKind::Invite, invite_token);
        let invite = match self.store.find_usable_invite(&token_hash, now).await {
            Ok(invite) => invite,
            Err(StoreError::NotFound) => return Err(AuthError::InviteInvalid),
            Err(err) => return Err(err.into()),
        };
        match invite.kind {
            InviteKind::Bootstrap if self.store.any_admin_exists().await? => {
                Err(AuthError::InviteInvalid)
            }
            InviteKind::Bootstrap => Ok(InvitePreview::NewMember { display_name: None }),
            InviteKind::NewMember { display_name, .. } => {
                Ok(InvitePreview::NewMember { display_name })
            }
            InviteKind::Reenroll { .. } => Ok(InvitePreview::Reenroll),
        }
    }

    /// Starts redeeming the invite behind `invite_token`. A setup or
    /// new-member invite needs a free `username` and a display name
    /// (`display_name`, else the invite's suggestion); a re-enroll invite
    /// ignores both.
    ///
    /// # Errors
    ///
    /// [`AuthError::InviteInvalid`]; [`AuthError::Invalid`] for a missing
    /// username or display name; [`AuthError::UsernameTaken`];
    /// [`AuthError::Busy`]; storage failures.
    pub async fn begin_enrollment(
        &self,
        invite_token: &str,
        username: Option<Username>,
        display_name: Option<DisplayName>,
    ) -> Result<(CeremonyId, CreationChallengeResponse), AuthError> {
        let now = self.clock.now();
        let token_hash = self.key.hash(TokenKind::Invite, invite_token);
        let invite = match self.store.find_usable_invite(&token_hash, now).await {
            Ok(invite) => invite,
            Err(StoreError::NotFound) => return Err(AuthError::InviteInvalid),
            Err(err) => return Err(err.into()),
        };
        let (role, suggestion, bootstrap) = match invite.kind {
            InviteKind::Bootstrap => {
                // Same predicate as `admin_exists`, read outside a transaction.
                if self.store.any_admin_exists().await? {
                    return Err(AuthError::InviteInvalid);
                }
                (Role::Admin, None, true)
            }
            InviteKind::NewMember { role, display_name } => (role, display_name, false),
            InviteKind::Reenroll { member } => {
                return self
                    .begin_reenrollment(invite.id, token_hash, invite.expires_at, member, now)
                    .await;
            }
        };
        let username = username.ok_or(AuthError::Invalid(IdentityError::InvalidUsername))?;
        let display_name = display_name
            .or(suggestion)
            .ok_or(AuthError::Invalid(IdentityError::InvalidDisplayName))?;
        match self.store.get_member_by_username(&username).await {
            Ok(_) => return Err(AuthError::UsernameTaken),
            Err(StoreError::NotFound) => {}
            Err(err) => return Err(err.into()),
        }
        let member = MemberId::new();
        let (options, state) =
            self.passkeys
                .start_registration(member, &username, &display_name, Vec::new())?;
        let ceremony = Ceremony::Enrollment {
            invite: invite.id,
            token_hash,
            invite_expires_at: invite.expires_at,
            target: EnrollTarget::New {
                member,
                username,
                display_name,
                role,
                bootstrap,
            },
            state: Box::new(state),
        };
        Ok((self.passkeys.insert(ceremony, now)?, options))
    }

    async fn begin_reenrollment(
        &self,
        invite: InviteId,
        token_hash: [u8; 32],
        invite_expires_at: Timestamp,
        member: MemberId,
        now: Timestamp,
    ) -> Result<(CeremonyId, CreationChallengeResponse), AuthError> {
        let target = match self.store.get_member(member).await {
            Ok(target) if target.disabled_at.is_none() => target,
            Ok(_) | Err(StoreError::NotFound) => return Err(AuthError::InviteInvalid),
            Err(err) => return Err(err.into()),
        };
        let exclude = self.credential_ids(member).await?;
        let (options, state) = self.passkeys.start_registration(
            member,
            &target.username,
            &target.display_name,
            exclude,
        )?;
        let ceremony = Ceremony::Enrollment {
            invite,
            token_hash,
            invite_expires_at,
            target: EnrollTarget::Existing(member),
            state: Box::new(state),
        };
        Ok((self.passkeys.insert(ceremony, now)?, options))
    }

    /// Finishes an enrollment: in one transaction, creates the member (setup
    /// ⇒ admin, new-member ⇒ the invite's role) or picks the re-enroll
    /// target, spends the invite, stores the passkey and audits it all, with
    /// the enrolling member as the actor.
    ///
    /// # Errors
    ///
    /// [`AuthError::CeremonyExpired`]; [`AuthError::PasskeyRejected`];
    /// [`AuthError::InviteUsed`] when the invite was redeemed meanwhile;
    /// [`AuthError::InviteInvalid`] when it expired, its target became
    /// unusable or its issuer is no longer an active admin;
    /// [`AuthError::UsernameTaken`];
    /// [`AuthError::PasskeyAlreadyRegistered`]; storage failures.
    pub async fn finish_enrollment(
        &self,
        ceremony: CeremonyId,
        label: TokenName,
        cred: RegisterPublicKeyCredential,
    ) -> Result<MemberId, AuthError> {
        let now = self.clock.now();
        let Ceremony::Enrollment {
            invite,
            token_hash,
            invite_expires_at,
            target,
            state,
        } = self.passkeys.take(&ceremony, now)?
        else {
            return Err(AuthError::CeremonyExpired);
        };
        let passkey = self.passkeys.finish_registration(&cred, &state)?;

        let mut tx = self.store.begin().await?;
        let usable = match tx.find_usable_invite(&token_hash, now).await {
            Ok(usable) => usable,
            Err(StoreError::NotFound) if now >= invite_expires_at => {
                return Err(AuthError::InviteInvalid);
            }
            Err(StoreError::NotFound) => return Err(AuthError::InviteUsed),
            Err(err) => return Err(err.into()),
        };
        // Demoting or disabling an admin revokes their invites; checked again
        // here so a link never outlives its issuer's admin rights.
        if let Some(creator) = usable.created_by
            && !is_active_admin(&tx.get_member(creator).await?)
        {
            return Err(AuthError::InviteInvalid);
        }
        let member = match target {
            EnrollTarget::New {
                member,
                username,
                display_name,
                role,
                bootstrap,
            } => {
                if bootstrap && admin_exists(&mut tx).await? {
                    return Err(AuthError::InviteInvalid);
                }
                let changes = json!({
                    "username": username.as_str(),
                    "display_name": display_name.as_str(),
                    "role": role.as_str(),
                    "invite": invite,
                });
                let created = tx
                    .insert_member(NewMember {
                        id: member,
                        username,
                        display_name,
                        role,
                        created_at: now,
                    })
                    .await
                    .map_err(|err| match err {
                        StoreError::Conflict(_) => AuthError::UsernameTaken,
                        other => other.into(),
                    })?;
                audit(
                    &mut tx,
                    now,
                    &member_actor(created.id),
                    "member.create",
                    "member",
                    created.id,
                    changes,
                )
                .await?;
                created.id
            }
            EnrollTarget::Existing(member) => match tx.get_member(member).await {
                Ok(target) if target.disabled_at.is_none() => target.id,
                Ok(_) | Err(StoreError::NotFound) => return Err(AuthError::InviteInvalid),
                Err(err) => return Err(err.into()),
            },
        };
        tx.consume_invite(invite, member, now)
            .await
            .map_err(|err| match err {
                StoreError::Conflict(_) => AuthError::InviteUsed,
                other => other.into(),
            })?;
        add_passkey(&mut tx, now, member, &label, &passkey, Some(invite)).await?;
        tx.commit().await?;
        Ok(member)
    }

    /// Starts registering another passkey for the actor.
    ///
    /// # Errors
    ///
    /// [`AuthError::Denied`] without [`Action::ManageOwnCredentials`] (agents
    /// never have it); [`AuthError::MemberDisabled`]; [`AuthError::Busy`];
    /// storage failures.
    pub async fn begin_add_passkey(
        &self,
        actor: &Actor,
    ) -> Result<(CeremonyId, CreationChallengeResponse), AuthError> {
        authorize(actor, Action::ManageOwnCredentials)?;
        let now = self.clock.now();
        let member = self.store.get_member(actor.member).await?;
        if member.disabled_at.is_some() {
            return Err(AuthError::MemberDisabled);
        }
        let exclude = self.credential_ids(member.id).await?;
        let (options, state) = self.passkeys.start_registration(
            member.id,
            &member.username,
            &member.display_name,
            exclude,
        )?;
        let ceremony = Ceremony::AddPasskey {
            member: member.id,
            state: Box::new(state),
        };
        Ok((self.passkeys.insert(ceremony, now)?, options))
    }

    /// Finishes registering another passkey for the actor.
    ///
    /// # Errors
    ///
    /// [`AuthError::Denied`]; [`AuthError::CeremonyExpired`] (also for a
    /// ceremony begun by someone else); [`AuthError::PasskeyRejected`];
    /// [`AuthError::MemberDisabled`]; [`AuthError::PasskeyAlreadyRegistered`];
    /// storage failures.
    pub async fn finish_add_passkey(
        &self,
        actor: &Actor,
        ceremony: CeremonyId,
        label: TokenName,
        cred: RegisterPublicKeyCredential,
    ) -> Result<PasskeyId, AuthError> {
        authorize(actor, Action::ManageOwnCredentials)?;
        let now = self.clock.now();
        let Ceremony::AddPasskey { member, state } = self.passkeys.take(&ceremony, now)? else {
            return Err(AuthError::CeremonyExpired);
        };
        if member != actor.member {
            return Err(AuthError::CeremonyExpired);
        }
        let passkey = self.passkeys.finish_registration(&cred, &state)?;
        let mut tx = self.store.begin().await?;
        if tx.get_member(member).await?.disabled_at.is_some() {
            return Err(AuthError::MemberDisabled);
        }
        let id = add_passkey(&mut tx, now, member, &label, &passkey, None).await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Removes one of the actor's passkeys, never the last one.
    ///
    /// # Errors
    ///
    /// [`AuthError::Denied`]; [`AuthError::NotFound`] (also for another
    /// member's passkey); [`AuthError::LastPasskey`]; storage failures.
    pub async fn remove_passkey(&self, actor: &Actor, id: PasskeyId) -> Result<(), AuthError> {
        authorize(actor, Action::ManageOwnCredentials)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let removed = tx.get_passkey(actor.member, id).await?;
        tx.delete_passkey(actor.member, id).await?;
        if tx.count_passkeys(actor.member).await? == 0 {
            return Err(AuthError::LastPasskey);
        }
        audit(
            &mut tx,
            now,
            &actor.into(),
            "passkey.remove",
            "passkey",
            id,
            json!({ "label": { "from": removed.label, "to": null } }),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Starts signing in `username`. Sign-in is username-first: the options
    /// list that member's credentials.
    ///
    /// # Errors
    ///
    /// [`AuthError::LoginFailed`] for an unknown or disabled member, or one
    /// without passkeys; [`AuthError::Busy`]; storage failures.
    pub async fn begin_login(
        &self,
        username: &Username,
    ) -> Result<(CeremonyId, RequestChallengeResponse), AuthError> {
        let now = self.clock.now();
        let member = match self.store.get_member_by_username(username).await {
            Ok(member) if member.disabled_at.is_none() => member,
            Ok(_) | Err(StoreError::NotFound) => return Err(AuthError::LoginFailed),
            Err(err) => return Err(err.into()),
        };
        let passkeys = self
            .store
            .list_passkeys(member.id)
            .await?
            .iter()
            .map(decode_passkey)
            .collect::<Result<Vec<_>, _>>()?;
        if passkeys.is_empty() {
            return Err(AuthError::LoginFailed);
        }
        let (options, state) = self.passkeys.start_authentication(&passkeys)?;
        let ceremony = Ceremony::Login {
            member: member.id,
            state: Box::new(state),
        };
        Ok((self.passkeys.insert(ceremony, now)?, options))
    }

    /// Verifies a sign-in assertion, stores the credential's new state and
    /// audits `session.login`.
    ///
    /// # Errors
    ///
    /// [`AuthError::CeremonyExpired`]; [`AuthError::LoginFailed`] for any
    /// assertion that does not verify or a member disabled meanwhile;
    /// [`AuthError::Webauthn`] for a verification failure on the server's
    /// side; storage failures.
    pub async fn finish_login(
        &self,
        ceremony: CeremonyId,
        cred: PublicKeyCredential,
    ) -> Result<MemberId, AuthError> {
        let now = self.clock.now();
        let Ceremony::Login { member, state } = self.passkeys.take(&ceremony, now)? else {
            return Err(AuthError::CeremonyExpired);
        };
        let result = match self.passkeys.finish_authentication(&cred, &state) {
            Ok(result) => result,
            Err(AuthError::PasskeyRejected(cause)) => {
                if matches!(cause, WebauthnError::CredentialPossibleCompromise) {
                    self.warn_counter_regression(member, &cred).await;
                }
                return Err(AuthError::LoginFailed);
            }
            Err(err) => return Err(err),
        };
        let passkey_id = self
            .store
            .list_passkeys(member)
            .await?
            .into_iter()
            .find(|record| record.credential_id.as_slice() == result.cred_id().as_ref())
            .ok_or(AuthError::LoginFailed)?
            .id;

        let mut tx = self.store.begin().await?;
        match tx.get_member(member).await {
            Ok(current) if current.disabled_at.is_none() => {}
            Ok(_) | Err(StoreError::NotFound) => return Err(AuthError::LoginFailed),
            Err(err) => return Err(err.into()),
        }
        // Re-read inside the transaction so a concurrent sign-in's update is
        // not lost; `update_credential` only ever raises the counter.
        let record = match tx.get_passkey(member, passkey_id).await {
            Ok(record) => record,
            Err(StoreError::NotFound) => return Err(AuthError::LoginFailed),
            Err(err) => return Err(err.into()),
        };
        let mut passkey = decode_passkey(&record)?;
        passkey.update_credential(&result);
        tx.update_passkey_after_use(record.id, encode_passkey(&passkey)?, now)
            .await?;
        audit(
            &mut tx,
            now,
            &member_actor(member),
            "session.login",
            "member",
            member,
            json!({ "passkey": record.id }),
        )
        .await?;
        tx.commit().await?;
        Ok(member)
    }

    /// Changes `member`'s role, refusing to demote the last active admin.
    /// Demoting an admin revokes, in the same transaction, the invites they
    /// issued that are still pending: only an admin's links may be redeemed.
    ///
    /// # Errors
    ///
    /// [`AuthError::Denied`] without [`Action::ManageMembers`];
    /// [`AuthError::NotFound`]; [`AuthError::LastAdmin`]; storage failures.
    pub async fn set_role(
        &self,
        actor: &Actor,
        member: MemberId,
        role: Role,
    ) -> Result<(), AuthError> {
        authorize(actor, Action::ManageMembers)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let target = tx.get_member(member).await?;
        if target.role == role {
            return Ok(());
        }
        if is_active_admin(&target) && tx.count_active_admins().await? <= 1 {
            return Err(AuthError::LastAdmin);
        }
        tx.set_member_role(member, role).await?;
        if target.role == Role::Admin {
            revoke_invites_created_by(&mut tx, now, &actor.into(), member).await?;
        }
        let changes = json!({ "from": target.role.as_str(), "to": role.as_str() });
        audit(
            &mut tx,
            now,
            &actor.into(),
            "member.role",
            "member",
            member,
            changes,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Disables `member` and, in the same transaction, deletes their
    /// sessions and revokes their agent tokens and their pending invites.
    /// Refuses the last active admin.
    ///
    /// # Errors
    ///
    /// [`AuthError::Denied`] without [`Action::ManageMembers`];
    /// [`AuthError::NotFound`]; [`AuthError::MemberDisabled`] if already
    /// disabled; [`AuthError::LastAdmin`]; storage failures.
    pub async fn disable_member(&self, actor: &Actor, member: MemberId) -> Result<(), AuthError> {
        authorize(actor, Action::ManageMembers)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let target = tx.get_member(member).await?;
        if target.disabled_at.is_some() {
            return Err(AuthError::MemberDisabled);
        }
        if target.role == Role::Admin && tx.count_active_admins().await? <= 1 {
            return Err(AuthError::LastAdmin);
        }
        tx.set_member_disabled(member, now).await?;
        tx.delete_sessions_for_member(member).await?;
        tx.revoke_tokens_for_member(member, now).await?;
        revoke_invites_created_by(&mut tx, now, &actor.into(), member).await?;
        audit(
            &mut tx,
            now,
            &actor.into(),
            "member.disable",
            "member",
            member,
            json!({ "disabled_at": { "from": null, "to": now.to_string() } }),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Logs a signature counter that went backwards: the credential may have
    /// been cloned. Only ids are logged, never credential data.
    async fn warn_counter_regression(&self, member: MemberId, cred: &PublicKeyCredential) {
        let passkey = match self.store.list_passkeys(member).await {
            Ok(records) => records
                .into_iter()
                .find(|record| record.credential_id.as_slice() == cred.raw_id.as_ref())
                .map(|record| record.id.to_string()),
            Err(_) => None,
        };
        tracing::warn!(
            member = %member,
            passkey = passkey.as_deref().unwrap_or("unknown"),
            "passkey signature counter went backwards; the credential may be cloned"
        );
    }

    /// Mints an invite token, stores its hash with `kind`, audits
    /// `invite.create` and returns `<public_url>/<path>/<token>`.
    async fn issue_invite(
        &self,
        tx: &mut StoreTx,
        now: Timestamp,
        kind: InviteKind,
        created_by: Option<MemberId>,
        actor: &AuditActor,
        path: &str,
    ) -> Result<Url, AuthError> {
        let token = mint(TokenKind::Invite);
        let expires_at = now
            .checked_add(self.config.invite_ttl)
            .map_err(|_| AuthError::Internal("invite expiry out of range"))?;
        let mut changes = invite_changes(&kind);
        changes["expires_at"] = json!(expires_at.to_string());
        let invite = tx
            .insert_invite(NewInvite {
                token_hash: self.key.hash(TokenKind::Invite, token.expose_secret()),
                kind,
                created_by,
                created_at: now,
                expires_at,
            })
            .await?;
        audit(
            tx,
            now,
            actor,
            "invite.create",
            "invite",
            invite.id,
            changes,
        )
        .await?;
        self.link(path, &token)
    }

    fn link(&self, path: &str, token: &SecretToken) -> Result<Url, AuthError> {
        // The configured public URL is a bare origin (see `Config`).
        let mut url = self.config.public_url.clone();
        url.path_segments_mut()
            .map_err(|()| AuthError::Internal("public URL cannot carry a path"))?
            .pop_if_empty()
            .push(path)
            .push(token.expose_secret());
        Ok(url)
    }

    /// Credential ids `member` already has, to exclude from registration.
    async fn credential_ids(&self, member: MemberId) -> Result<Vec<CredentialID>, AuthError> {
        self.store
            .list_passkeys(member)
            .await?
            .iter()
            .map(|record| decode_passkey(record).map(|passkey| passkey.cred_id().clone()))
            .collect()
    }
}

/// Whether the household has an active admin: the one predicate that ends
/// bootstrap.
async fn admin_exists(tx: &mut StoreTx) -> Result<bool, AuthError> {
    Ok(tx.count_active_admins().await? > 0)
}

fn is_active_admin(member: &coffer_store::Member) -> bool {
    member.role == Role::Admin && member.disabled_at.is_none()
}

/// Revokes every pending invite `creator` issued, auditing each with
/// `actor`.
async fn revoke_invites_created_by(
    tx: &mut StoreTx,
    now: Timestamp,
    actor: &AuditActor,
    creator: MemberId,
) -> Result<(), AuthError> {
    for invite in tx.list_pending_invites(now).await? {
        if invite.created_by == Some(creator) {
            tx.revoke_invite(invite.id, now).await?;
            audit(
                tx,
                now,
                actor,
                "invite.revoke",
                "invite",
                invite.id,
                json!({ "reason": "issuer no longer an admin" }),
            )
            .await?;
        }
    }
    Ok(())
}

fn member_actor(member: MemberId) -> AuditActor {
    AuditActor::Member {
        member,
        agent: None,
    }
}

fn invite_changes(kind: &InviteKind) -> Value {
    match kind {
        InviteKind::Bootstrap => json!({ "kind": "bootstrap" }),
        InviteKind::NewMember { role, display_name } => json!({
            "kind": "new_member",
            "role": role.as_str(),
            "display_name": display_name.as_ref().map(DisplayName::as_str),
        }),
        InviteKind::Reenroll { member } => json!({ "kind": "reenroll", "member": member }),
    }
}

/// Stores `passkey` for `member` and audits `passkey.add` with the member as
/// the actor. Only the label and the invite (if any) are recorded.
async fn add_passkey(
    tx: &mut StoreTx,
    now: Timestamp,
    member: MemberId,
    label: &TokenName,
    passkey: &Passkey,
    invite: Option<InviteId>,
) -> Result<PasskeyId, AuthError> {
    let record = tx
        .insert_passkey(NewPasskey {
            member,
            label: label.as_str().to_owned(),
            credential_id: passkey.cred_id().as_ref().to_vec(),
            credential_json: encode_passkey(passkey)?,
            created_at: now,
        })
        .await
        .map_err(passkey_insert_error)?;
    let changes = json!({ "member": member, "label": label.as_str(), "invite": invite });
    audit(
        tx,
        now,
        &member_actor(member),
        "passkey.add",
        "passkey",
        record.id,
        changes,
    )
    .await?;
    Ok(record.id)
}

/// The credential id is unique: registering one that exists is a client
/// error, not a storage failure.
fn passkey_insert_error(err: StoreError) -> AuthError {
    match err {
        StoreError::Conflict(_) => AuthError::PasskeyAlreadyRegistered,
        other => other.into(),
    }
}

fn decode_passkey(record: &PasskeyRecord) -> Result<Passkey, AuthError> {
    serde_json::from_str(&record.credential_json)
        .map_err(|_| AuthError::Internal("stored passkey unreadable"))
}

fn encode_passkey(passkey: &Passkey) -> Result<String, AuthError> {
    serde_json::to_string(passkey).map_err(|_| AuthError::Internal("passkey not encodable"))
}

#[cfg(test)]
mod tests {
    use coffer_store::StoreError;

    use super::passkey_insert_error;
    use crate::auth::AuthError;

    #[test]
    fn duplicate_credential_is_a_client_error() {
        assert!(matches!(
            passkey_insert_error(StoreError::Conflict("unique constraint violated")),
            AuthError::PasskeyAlreadyRegistered
        ));
        assert!(matches!(
            passkey_insert_error(StoreError::InvalidCursor),
            AuthError::Store(_)
        ));
    }
}
