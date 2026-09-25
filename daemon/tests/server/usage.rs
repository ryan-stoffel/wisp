//! `usage/get` against a real server: it is answered even with an empty store.

use wisp_protocol::UsageGetParams;
use wisp_protocol::methods::UsageGet;

use crate::support::{Client, Wispd, temp_dir};

#[tokio::test]
async fn usage_get_reports_no_accounts_when_nothing_has_been_recorded() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::ready(&wispd.socket).await;

    let result = client.call::<UsageGet>(UsageGetParams {}).await.unwrap();
    assert!(result.accounts.is_empty());
}
