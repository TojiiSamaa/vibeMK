//! tools/call of every folder tool against a fake CheckMK.
mod common;

use serde_json::{json, Value};
use vibemk::handlers::base::Handler;
use vibemk::handlers::folders::FolderHandler;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

fn text(blocks: &[Value]) -> String {
    blocks[0]["text"].as_str().unwrap().to_string()
}

fn api(p: &str) -> String {
    format!("{}/{p}", common::API)
}

#[tokio::test]
async fn get_folders_lists_path_and_title() {
    let (server, client) = common::fake_checkmk().await;
    let body = json!({"value": [
        {"id": "~web", "title": "Web", "extensions": {"path": "web"}},
        {"id": "~db"}
    ]});
    Mock::given(method("GET"))
        .and(path(api("domain-types/folder_config/collections/all")))
        .and(query_param("parent", "~web"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(&server)
        .await;
    let out = FolderHandler::new(client)
        .handle("vibemk_get_folders", &json!({"parent": "~web"}))
        .await;
    assert_eq!(
        text(&out),
        "📁 **CheckMK Folders** (2 total, showing first 2):\n\n📁 web - Web\n📁 ~db - ~db"
    );
}

#[tokio::test]
async fn get_folders_says_when_empty() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .mount(&server)
        .await;
    let out = FolderHandler::new(client)
        .handle("vibemk_get_folders", &json!({}))
        .await;
    assert_eq!(text(&out), "📁 No folders found");
}

#[tokio::test]
async fn create_folder_posts_the_validated_path() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("POST"))
        .and(path(api("domain-types/folder_config/collections/all")))
        .and(body_json(
            json!({"name": "db", "title": "Databases", "parent": "~linux", "attributes": {}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let out = FolderHandler::new(client)
        .handle(
            "vibemk_create_folder",
            &json!({"folder": "DB", "title": "Databases", "parent": "/linux"}),
        )
        .await;
    assert_eq!(
        text(&out),
        "✅ **Folder Created Successfully**\n\n📁 **Folder:** db\n📝 **Title:** Databases\n📂 **Parent:** /linux\n🔗 **Full Path:** /linux/db\n⚙️ **API Path:** ~linux~db\n\n⚠️ **Remember to activate changes!**"
    );
}

#[tokio::test]
async fn create_folder_refuses_a_path_as_name_and_missing_arguments() {
    let (server, client) = common::fake_checkmk().await;
    let h = FolderHandler::new(client);
    let out = h
        .handle(
            "vibemk_create_folder",
            &json!({"folder": "a/b", "title": "T"}),
        )
        .await;
    assert_eq!(text(&out), "❌ **Invalid folder name**\n\nFolder name cannot contain path separators. Use 'parent' parameter for folder hierarchy.");
    assert_eq!(out[1]["type"], "_error");
    let out = h
        .handle("vibemk_create_folder", &json!({"folder": "a"}))
        .await;
    assert_eq!(
        text(&out),
        "❌ **Missing parameters**\n\nfolder and title are required"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn delete_folder_sends_the_delete_mode() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("DELETE"))
        .and(path(api("objects/folder_config/~linux~web")))
        .and(query_param("delete_mode", "recursive"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let out = FolderHandler::new(client)
        .handle(
            "vibemk_delete_folder",
            &json!({"folder": "/linux/web", "delete_mode": "recursive"}),
        )
        .await;
    assert!(text(&out).starts_with("✅ **Folder Deleted Successfully**\n\n📁 **Folder:** /linux/web\n⚙️ **API Path:** ~linux~web\n🗑️ **Delete Mode:** recursive\n"), "{}", text(&out));
}

#[tokio::test]
async fn delete_folder_refuses_a_reserved_name() {
    let (server, client) = common::fake_checkmk().await;
    let out = FolderHandler::new(client)
        .handle("vibemk_delete_folder", &json!({"folder": "/tmp"}))
        .await;
    assert_eq!(
        text(&out),
        "❌ **Invalid folder path**\n\n❌ 'tmp' is a reserved folder name"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_dotted_folder_name_cannot_navigate_the_api() {
    // Injection check: ".." is not a valid folder name, so no request leaves.
    let (server, client) = common::fake_checkmk().await;
    let out = FolderHandler::new(client)
        .handle(
            "vibemk_get_folder_hosts",
            &json!({"folder": "/../objects/user_config/admin"}),
        )
        .await;
    assert_eq!(out[1]["type"], "_error");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn update_folder_puts_with_the_current_etag() {
    let (server, client) = common::fake_checkmk().await;
    let p = api("objects/folder_config/~web");
    Mock::given(method("GET"))
        .and(path(p.clone()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"e1\"")
                .set_body_json(json!({})),
        )
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(p))
        .and(header("If-Match", "\"e1\""))
        .and(body_json(
            json!({"title": "New", "attributes": {"a": 1, "b": 2}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let out = FolderHandler::new(client)
        .handle(
            "vibemk_update_folder",
            &json!({"folder": "web", "title": "New", "attributes": {"a": 1, "b": 2}}),
        )
        .await;
    assert_eq!(
        text(&out),
        "✅ **Folder Updated Successfully**\n\n📁 **Folder:** /web\n⚙️ **API Path:** ~web\n📝 **New Title:** New\n⚙️ **Attributes Updated:** 2 items\n\n⚠️ **Remember to activate changes!**"
    );
}

#[tokio::test]
async fn update_folder_needs_something_to_change() {
    let (_server, client) = common::fake_checkmk().await;
    let out = FolderHandler::new(client)
        .handle("vibemk_update_folder", &json!({"folder": "web"}))
        .await;
    assert_eq!(
        text(&out),
        "❌ **Missing parameters**\n\nAt least one of 'title' or 'attributes' is required"
    );
}

#[tokio::test]
async fn move_folder_posts_the_destination() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("POST"))
        .and(path(api("objects/folder_config/~web/actions/move/invoke")))
        .and(body_json(json!({"destination": "~prod"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let out = FolderHandler::new(client)
        .handle(
            "vibemk_move_folder",
            &json!({"folder": "web", "destination": "prod"}),
        )
        .await;
    assert_eq!(
        text(&out),
        "✅ **Folder Moved Successfully**\n\n📁 **Source:** /web\n📂 **Destination:** /prod\n⚙️ **API Paths:**\n   • From: ~web\n   • To: ~prod\n\n⚠️ **Remember to activate changes!**"
    );
}

#[tokio::test]
async fn get_folder_hosts_lists_hosts_and_empty_folders() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET")).and(path(api("objects/folder_config/~web/collections/hosts"))).respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [{"id": "web1", "extensions": {"alias": "First"}}, {"id": "web2", "extensions": {}}]}))).mount(&server).await;
    Mock::given(method("GET"))
        .and(path(api("objects/folder_config/~empty/collections/hosts")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .mount(&server)
        .await;
    let h = FolderHandler::new(client);
    let out = h
        .handle("vibemk_get_folder_hosts", &json!({"folder": "/web"}))
        .await;
    assert_eq!(text(&out), "📁 **Hosts in Folder '/web'** (2 total):\n\n⚙️ **API Path:** ~web\n\n🖥️ web1 (First)\n🖥️ web2");
    let out = h
        .handle("vibemk_get_folder_hosts", &json!({"folder": "empty"}))
        .await;
    assert_eq!(text(&out), "📁 **Folder '/empty' is empty**\n\n⚙️ **API Path:** ~empty\n\nNo hosts found in this folder.");
}

#[tokio::test]
async fn an_api_error_and_an_unknown_tool_are_reported() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({"title": "boom"})))
        .mount(&server)
        .await;
    let h = FolderHandler::new(client);
    let out = h.handle("vibemk_get_folders", &json!({})).await;
    assert!(
        text(&out).starts_with("❌ **CheckMK API Error**"),
        "{}",
        text(&out)
    );
    let out = h.handle("nope", &json!({})).await;
    assert_eq!(
        text(&out),
        "❌ **Unknown tool**\n\nTool 'nope' is not supported"
    );
}
