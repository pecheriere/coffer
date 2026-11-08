//! A missing emergency fund points an admin to where it is set, and tells anyone else who
//! can set it, never sending them to a page they can't open.

use coffer_domain::identity::Role;
use coffer_server::testkit::browser::Browser;

use super::{PAGE, signed_in, text_of};

const LINK: &str = "href=\"/admin/household#emergency-fund\"";

#[tokio::test]
async fn blocker_links_only_for_admins() {
    let world = crate::forecast::world().await;
    world.funded("Checking", 123_456).await;
    let invite = world
        .server
        .identity
        .create_invite(&world.alice, Role::Member, None)
        .await
        .unwrap();
    let mut bob = Browser::new(&world.server);
    let mut key = world.server.authenticator();
    bob.enroll(&mut key, &invite, "bob", "Bob").await;

    let admin = signed_in(&world).await.get(PAGE).await.body;
    let member = bob.get(PAGE).await.body;

    assert!(admin.contains(LINK));
    assert!(text_of(&admin).contains("No emergency fund is set for USD"));
    assert!(text_of(&admin).contains("Set one on the Household page"));
    assert!(!member.contains(LINK));
    assert!(!member.contains("/admin/household"));
    assert!(text_of(&member).contains("An admin can set one on the Household page."));
    for body in [&admin, &member] {
        assert!(!body.contains("through the API"));
    }
}
