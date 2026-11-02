use secrecy::{ExposeSecret as _, SecretString};

use super::{
    AccessTokenFields, AccountsAnswer, MAX_ACCOUNTS, MAX_PAGE_RECORDS, PlaidAccount, PlaidClient,
    PlaidError, PlaidRequest, PlaidTransaction, RawSyncPage, SyncPage, code_or_unknown, read,
};
use crate::config::{PlaidConfig, PlaidEnvironment};
use crate::egress::{Egress, EgressError, HostAllowList};
use crate::sync::apply::MAX_PROVIDER_ID_CHARS;

fn config(environment: PlaidEnvironment) -> PlaidConfig {
    PlaidConfig {
        client_id: "CLIENT-MARKER".to_owned(),
        secret: SecretString::from("SECRET-MARKER"),
        environment,
        country_codes: vec!["US".to_owned()],
    }
}

fn record(amount: &str) -> String {
    format!(
        r#"{{"transaction_id":"t","account_id":"a","amount":{amount},"date":"2027-01-10",
            "name":"n","pending":false}}"#
    )
}

#[test]
fn amounts_parsed_from_raw_numbers() {
    for (raw, exact) in [
        ("12", "12"),
        ("4.5", "4.5"),
        ("-4.50", "-4.50"),
        ("0.1", "0.1"),
    ] {
        let parsed = PlaidTransaction::parse(&record(raw)).unwrap();
        assert_eq!(parsed.amount.as_str(), exact, "the exact text is kept");
    }
    for raw in [
        "1e2",
        "1E2",
        "1.5e-3",
        "\"12\"",
        "null",
        "true",
        "[12]",
        "1234567890123456789012",
    ] {
        assert!(
            matches!(
                PlaidTransaction::parse(&record(raw)),
                Err(PlaidError::Invalid(_))
            ),
            "only an exact decimal number is read"
        );
    }
}

#[test]
fn other_fields_of_a_record_are_checked() {
    for bad in [
        r#"{"transaction_id":"t"}"#,
        r#"{"transaction_id":"t","account_id":"a","amount":1,"date":"not a date",
            "name":"n","pending":false}"#,
        r#"{"transaction_id":"t","account_id":"a","amount":1,"date":"2027-02-30",
            "name":"n","pending":false}"#,
        "7",
    ] {
        assert!(
            matches!(PlaidTransaction::parse(bad), Err(PlaidError::Invalid(_))),
            "a malformed record is invalid"
        );
    }
}

/// However long the list, no more than one entry past the limit is held,
/// and the rest must still be valid JSON.
#[test]
fn an_account_list_is_held_up_to_one_past_the_limit() {
    let list = |count: usize| format!(r#"{{"accounts":[{}]}}"#, vec!["{}"; count].join(","));
    let held = |count: usize| {
        let body = list(count);
        read::<AccountsAnswer<'_>>(body.as_bytes())
            .unwrap()
            .accounts
            .len()
    };
    assert_eq!(held(0), 0);
    assert_eq!(held(MAX_ACCOUNTS), MAX_ACCOUNTS);
    assert_eq!(held(MAX_ACCOUNTS + 1), MAX_ACCOUNTS + 1);
    assert_eq!(held(MAX_ACCOUNTS * 5), MAX_ACCOUNTS + 1);
    let broken = format!(
        r#"{{"accounts":[{},oops]}}"#,
        vec!["{}"; MAX_ACCOUNTS * 2].join(",")
    );
    assert!(
        read::<AccountsAnswer<'_>>(broken.as_bytes()).is_err(),
        "malformed JSON past the limit is still refused"
    );
}

/// Each list of a page is held up to one past the limit, whatever its length,
/// and the rest must still be valid JSON.
#[test]
fn a_page_list_is_held_up_to_one_past_the_limit() {
    let page = |count: usize| {
        format!(
            r#"{{"added":[{}],"modified":[],"removed":[],"next_cursor":"c","has_more":false}}"#,
            vec!["{}"; count].join(",")
        )
    };
    let held = |count: usize| {
        let body = page(count);
        read::<RawSyncPage<'_>>(body.as_bytes())
            .unwrap()
            .added
            .len()
    };
    assert_eq!(held(MAX_PAGE_RECORDS), MAX_PAGE_RECORDS);
    assert_eq!(held(MAX_PAGE_RECORDS * 5), MAX_PAGE_RECORDS + 1);
    let broken = page(MAX_PAGE_RECORDS * 2).replacen("{}]", "{},oops]", 1);
    assert!(
        read::<RawSyncPage<'_>>(broken.as_bytes()).is_err(),
        "malformed JSON past the limit is still refused"
    );
}

/// The limit is in characters, as the store's is, and holds for every id
/// a record carries.
#[test]
fn ids_are_limited_in_characters() {
    let parsed = |id: &str, account: &str, replaced: &str| {
        PlaidTransaction::parse(&format!(
            r#"{{"transaction_id":"{id}","account_id":"{account}","amount":1,
                "date":"2027-01-10","name":"n","pending":false,
                "pending_transaction_id":"{replaced}"}}"#
        ))
    };
    let longest = "é".repeat(MAX_PROVIDER_ID_CHARS);
    let long = "é".repeat(MAX_PROVIDER_ID_CHARS + 1);
    assert!(
        parsed(&longest, &longest, &longest).is_ok(),
        "an id at the limit is read"
    );
    for over in [parsed(&long, "a", "p"), parsed("t", &long, "p")] {
        assert!(
            matches!(over, Err(PlaidError::Invalid(_))),
            "an id of the record over the limit is invalid"
        );
    }
    assert_eq!(
        parsed("t", "a", &long).unwrap().pending_transaction_id,
        None,
        "an over-long reference is dropped and the record kept"
    );
    assert_eq!(
        parsed("t", "a", &longest).unwrap().pending_transaction_id,
        Some(longest),
        "a reference at the limit is kept"
    );
}

/// One rule for the ids of accounts: 1 to the limit, without control
/// characters, wherever an account id is read.
#[test]
fn account_ids_follow_one_validity_rule() {
    let transaction = |account: &str| {
        PlaidTransaction::parse(&format!(
            r#"{{"transaction_id":"t","account_id":"{account}","amount":1,
                "date":"2027-01-10","name":"n","pending":false}}"#
        ))
    };
    let removed = |account: &str| {
        let page = format!(
            r#"{{"added":[],"modified":[],"removed":[{{"transaction_id":"t","account_id":"{account}"}}],
                "next_cursor":"c","has_more":false}}"#
        );
        read::<RawSyncPage<'_>>(page.as_bytes())
            .map(|raw| SyncPage::from_raw(raw, 0))
            .unwrap()
    };
    let account = |id: &str| {
        PlaidAccount::parse(&format!(
            r#"{{"account_id":"{id}","name":"n","type":"depository",
                "balances":{{"current":1}}}}"#
        ))
    };
    assert!(transaction("a").is_ok() && account("a").is_ok());
    assert_eq!(removed("a").removed.len(), 1);
    for bad in ["", "a\\nb", "a\\u0007b", "a\\u001bb"] {
        assert!(
            matches!(transaction(bad), Err(PlaidError::Invalid(_))),
            "a record of an unusable account id is unreadable"
        );
        assert!(
            matches!(account(bad), Err(PlaidError::Invalid(_))),
            "so is an account"
        );
        let page = removed(bad);
        assert!(page.removed.is_empty(), "so is a removal");
        assert_eq!(page.unusable.len(), 1);
        assert_eq!(
            page.unusable[0].account_id, None,
            "and what is left of it names no account"
        );
    }
}

#[tokio::test]
async fn only_the_environment_host_is_reachable() {
    let client = PlaidClient {
        egress: Egress::new(None).unwrap(),
        config: config(PlaidEnvironment::Sandbox),
        base: "https://attacker.example/".parse().unwrap(),
        hosts: HostAllowList::parse(PlaidEnvironment::Sandbox.host()).unwrap(),
    };
    let token = SecretString::from("access-token");
    assert!(
        matches!(
            client.accounts(&token).await,
            Err(PlaidError::Egress(EgressError::HostNotAllowed))
        ),
        "another host must be refused"
    );

    // Each environment's client allows its own host, not the other's.
    for (environment, own, other) in [
        (
            PlaidEnvironment::Sandbox,
            "https://sandbox.plaid.com/accounts/get",
            "https://production.plaid.com/accounts/get",
        ),
        (
            PlaidEnvironment::Production,
            "https://production.plaid.com/accounts/get",
            "https://sandbox.plaid.com/accounts/get",
        ),
    ] {
        let client = PlaidClient::new(Egress::new(None).unwrap(), config(environment)).unwrap();
        assert_eq!(client.base.host_str(), Some(environment.host()));
        assert!(
            client
                .egress
                .check_url(&own.parse().unwrap(), &client.hosts)
                .is_ok()
        );
        assert!(matches!(
            client
                .egress
                .check_url(&other.parse().unwrap(), &client.hosts),
            Err(EgressError::HostNotAllowed)
        ));
    }
}

#[test]
fn debug_output_redacts_credentials() {
    let config = config(PlaidEnvironment::Sandbox);
    let token = SecretString::from("TOKEN-MARKER");
    let request = PlaidRequest {
        client_id: &config.client_id,
        secret: config.secret.expose_secret(),
        fields: AccessTokenFields {
            access_token: token.expose_secret(),
        },
    };
    let client = PlaidClient::new(Egress::new(None).unwrap(), config.clone()).unwrap();
    for debug in [format!("{request:?}"), format!("{client:?}")] {
        for marker in ["CLIENT-MARKER", "SECRET-MARKER", "TOKEN-MARKER"] {
            assert!(
                !debug.contains(marker),
                "the Debug output must not show a credential"
            );
        }
    }
}

#[test]
fn only_code_shaped_text_is_kept() {
    assert_eq!(code_or_unknown("INVALID_FIELD", &[]), "INVALID_FIELD");
    assert_eq!(code_or_unknown("HTTP_502", &[]), "HTTP_502");
    for bad in [
        "",
        "access-sandbox-1234",
        "lowercase",
        "WITH SPACE",
        "NEW\nLINE",
        &"A".repeat(65),
    ] {
        assert_eq!(code_or_unknown(bad, &[]), "UNKNOWN");
    }
    // A credential shaped like a code is withheld, alone or inside one.
    let credentials = ["UPPERCLIENT1", "UPPERSECRET2", ""];
    for echoed in ["UPPERCLIENT1", "UPPERSECRET2", "X_UPPERSECRET2_Y"] {
        assert!(
            code_or_unknown(echoed, &credentials) == "UNKNOWN",
            "a code holding a credential is replaced"
        );
    }
    assert_eq!(code_or_unknown("OTHER", &credentials), "OTHER");
}
