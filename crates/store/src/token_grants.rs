//! The account grants of agent tokens. A token without grants has uniform
//! access at its scope; the policy decides what grants mean.

use coffer_domain::ids::AgentTokenId;
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use sqlx::sqlite::SqliteRow;

use crate::convert::{corrupt, parsed, text};
use crate::{Store, StoreError, StoreTx};

fn level_from_str(level: &str) -> Result<AccountLevel, StoreError> {
    match level {
        "balances" => Ok(AccountLevel::Balances),
        "transactions" => Ok(AccountLevel::Transactions),
        "write" => Ok(AccountLevel::Write),
        _ => Err(corrupt("unknown grant level")),
    }
}

fn grant_from_row(row: &SqliteRow) -> Result<TokenGrant, StoreError> {
    Ok(TokenGrant {
        target: parsed::<GrantTarget>(row, "target")?,
        level: level_from_str(&text(row, "level")?)?,
    })
}

impl StoreTx {
    /// Replace every grant of `token` with `grants`; none leaves it with
    /// uniform access. The caller has validated them.
    ///
    /// # Errors
    ///
    /// `Conflict` if a target appears twice; backend failures, including an
    /// unknown token.
    pub async fn replace_token_grants(
        &mut self,
        token: AgentTokenId,
        grants: &[TokenGrant],
    ) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM agent_token_grants WHERE token_id = ?")
            .bind(token.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        for grant in grants {
            sqlx::query(
                "INSERT INTO agent_token_grants (token_id, target, level) VALUES (?, ?, ?)",
            )
            .bind(token.to_string())
            .bind(grant.target.to_string())
            .bind(grant.level.as_str())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        }
        Ok(())
    }
}

impl Store {
    /// The grants of `token`, `all` first, then by account id; empty for a
    /// token with uniform access (or an unknown one).
    ///
    /// # Errors
    ///
    /// Backend failures, or a stored grant that no longer parses: a grant
    /// is never dropped silently, since dropping the last one would widen
    /// the token to uniform access.
    pub async fn token_grants(&self, token: AgentTokenId) -> Result<Vec<TokenGrant>, StoreError> {
        let rows = sqlx::query(
            "SELECT target, level FROM agent_token_grants WHERE token_id = ? \
             ORDER BY target = 'all' DESC, target",
        )
        .bind(token.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from_sqlx)?;
        rows.iter().map(grant_from_row).collect()
    }
}
