mod common;

use std::collections::BTreeSet;

use vibemk::registry::ToolRegistry;

#[tokio::test]
async fn registry_routes_every_tool_of_tools_json() {
    let (_server, client) = common::fake_checkmk().await;
    let registry = ToolRegistry::from_client(client);

    let routed: BTreeSet<&str> = registry.tool_names().into_iter().collect();
    let listed: BTreeSet<&str> = vibemk::tools::tool_names().into_iter().collect();
    assert_eq!(routed.len(), 154);
    assert_eq!(listed.len(), 154);
    assert_eq!(routed, listed);

    let handler = registry
        .handler_for("vibemk_get_checkmk_hosts")
        .expect("routed");
    let blocks = handler
        .handle("vibemk_get_checkmk_hosts", &serde_json::json!({}))
        .await;
    assert!(vibemk::dispatch::is_error(&blocks));
    assert!(registry.handler_for("nope").is_none());
}
