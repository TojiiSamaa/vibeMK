//! tools/call of the 4 host tag tools against a fake CheckMK (method, path, body, If-Match checked).
mod common;

use serde_json::{json, Value};
use vibemk::handlers::base::Handler;
use vibemk::handlers::tags::TagsHandler;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API: &str = "/cmk/check_mk/api/1.0";
const ALL: &str = "/cmk/check_mk/api/1.0/domain-types/host_tag_group/collections/all";
const OBJ: &str = "/cmk/check_mk/api/1.0/objects/host_tag_group/dc";

fn text(b: &[Value]) -> String {
    b[0]["text"].as_str().unwrap().to_string()
}
fn is_error(b: &[Value]) -> bool {
    b.iter().any(|x| x["type"] == "_error")
}
async fn setup() -> (MockServer, TagsHandler) {
    let (server, client) = common::fake_checkmk().await;
    (server, TagsHandler::new(client))
}

#[tokio::test]
async fn get_host_tags_previews_three_tags() {
    let (s, h) = setup().await;
    let body = json!({"value": [
        {"id": "dc", "extensions": {"title": "DC", "tags": [{"id": "a", "title": "A"}, {"id": "b"}, {"title": "C"}, {"id": "d", "title": "D"}, {"id": "e", "title": "E"}]}},
        {"id": "bare", "extensions": {}},
    ]});
    Mock::given(method("GET"))
        .and(path(ALL))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&s)
        .await;
    assert_eq!(text(&h.handle("vibemk_get_host_tags", &json!({})).await), "🏷️ **Host Tag Groups** (2 total):\n\n🏷️ **dc** - DC\n   Tags: A, b, C, ... (+2 more)\n   Total: 5 tags\n\n🏷️ **bare** - bare\n   Tags: \n   Total: 0 tags");
    let (s, h) = setup().await;
    Mock::given(method("GET"))
        .and(path(ALL))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .mount(&s)
        .await;
    assert_eq!(
        text(&h.handle("vibemk_get_host_tags", &json!({})).await),
        "🏷️ **No Host Tag Groups Found**\n\nNo host tag groups are configured."
    );
}

#[tokio::test]
async fn create_posts_id_title_tags_and_optional_topic_help() {
    let (s, h) = setup().await;
    let tags = json!([{"id": "a", "title": "A"}]);
    Mock::given(method("POST"))
        .and(path(ALL))
        .and(body_json(
            json!({"id": "dc", "title": "DC", "tags": tags, "topic": "Net", "help": "hlp"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&s)
        .await;
    let out = h
        .handle(
            "vibemk_create_host_tag",
            &json!({"tag_id": "dc", "title": "DC", "tags": tags, "topic": "Net", "help": "hlp"}),
        )
        .await;
    assert_eq!(text(&out), "✅ **Host Tag Group Created Successfully**\n\nTag ID: dc\nTitle: DC\nTopic: Net\nTags: 1 configured\n\n⚠️ **Remember to activate changes!**");
    assert!(!is_error(&out));
}

#[tokio::test]
async fn create_without_topic_sends_no_topic_and_says_none() {
    let (s, h) = setup().await;
    let tags = json!([{"id": "a", "title": "A"}]);
    Mock::given(method("POST"))
        .and(path(ALL))
        .and(body_json(json!({"id": "dc", "title": "DC", "tags": tags})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&s)
        .await;
    let out = h
        .handle(
            "vibemk_create_host_tag",
            &json!({"tag_id": "dc", "title": "DC", "tags": tags}),
        )
        .await;
    assert!(text(&out).contains("Topic: None\n"));
}

#[tokio::test]
async fn create_validates_input_before_any_call() {
    let (s, h) = setup().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&s)
        .await;
    assert_eq!(
        text(
            &h.handle("vibemk_create_host_tag", &json!({"title": "x"}))
                .await
        ),
        "❌ **Missing parameters**\n\ntag_id and title are required"
    );
    assert_eq!(
        text(
            &h.handle(
                "vibemk_create_host_tag",
                &json!({"tag_id": "a", "title": "x"})
            )
            .await
        ),
        "❌ **Missing parameter**\n\ntags list is required (at least one tag)"
    );
    let out = h
        .handle(
            "vibemk_create_host_tag",
            &json!({"tag_id": "a", "title": "x", "tags": [{"id": "only"}]}),
        )
        .await;
    assert_eq!(
        text(&out),
        "❌ **Invalid tag structure**\n\nEach tag must have 'id' and 'title' fields"
    );
    assert!(is_error(&out));
}

#[tokio::test]
async fn update_puts_given_fields_with_the_etag() {
    let (s, h) = setup().await;
    Mock::given(method("GET"))
        .and(path(OBJ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"e1\"")
                .set_body_json(json!({})),
        )
        .mount(&s)
        .await;
    let tags = json!([{"id": "a", "title": "A"}]);
    Mock::given(method("PUT"))
        .and(path(OBJ))
        .and(header("If-Match", "\"e1\""))
        .and(body_json(
            json!({"title": "New", "topic": "", "tags": tags, "help": "h", "repair": true}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&s)
        .await;
    let out = h.handle("vibemk_update_host_tag", &json!({"tag_id": "dc", "title": "New", "topic": "", "tags": tags, "help": "h", "repair": true})).await;
    assert_eq!(text(&out), "✅ **Host Tag Group Updated Successfully**\n\nTag ID: dc\nUpdated fields: title, topic, tags, help, repair\n\n⚠️ **Remember to activate changes!**");
}

#[tokio::test]
async fn update_rejects_empty_and_invalid_input() {
    let (s, h) = setup().await;
    Mock::given(method("GET"))
        .and(path(OBJ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&s)
        .await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&s)
        .await;
    assert_eq!(
        text(&h.handle("vibemk_update_host_tag", &json!({})).await),
        "❌ **Missing parameter**\n\ntag_id is required"
    );
    assert_eq!(
        text(
            &h.handle("vibemk_update_host_tag", &json!({"tag_id": "dc"}))
                .await
        ),
        "❌ **No data to update**\n\nAt least one field must be provided"
    );
    assert_eq!(
        text(
            &h.handle(
                "vibemk_update_host_tag",
                &json!({"tag_id": "dc", "tags": ["x"]})
            )
            .await
        ),
        "❌ **Invalid tag structure**\n\nEach tag must have 'id' and 'title' fields"
    );
}

#[tokio::test]
async fn update_unknown_group_is_an_api_error() {
    let (s, h) = setup().await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/objects/host_tag_group/zz")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({})))
        .mount(&s)
        .await;
    let out = h
        .handle(
            "vibemk_update_host_tag",
            &json!({"tag_id": "zz", "title": "t"}),
        )
        .await;
    assert!(text(&out).starts_with("❌ **CheckMK API Error**"));
    assert!(is_error(&out));
}

#[tokio::test]
async fn delete_sends_repair_only_when_asked() {
    let (s, h) = setup().await;
    Mock::given(method("GET"))
        .and(path(OBJ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&s)
        .await;
    Mock::given(method("DELETE"))
        .and(path(OBJ))
        .and(query_param("repair", "true"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&s)
        .await;
    let out = h
        .handle(
            "vibemk_delete_host_tag",
            &json!({"tag_id": "dc", "repair": true}),
        )
        .await;
    assert_eq!(text(&out), "✅ **Host Tag Group Deleted Successfully**\n\nTag ID: dc\nRepair mode: Enabled\n\n📝 **Next Steps:**\n1️⃣ Use 'get_pending_changes' to review the deletion\n2️⃣ Use 'activate_changes' to apply the configuration\n\n💡 **Important:** The tag group is only marked for deletion until you activate changes!");
}

#[tokio::test]
async fn delete_without_repair_has_no_query() {
    let (s, h) = setup().await;
    Mock::given(method("GET"))
        .and(path(OBJ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&s)
        .await;
    Mock::given(method("DELETE"))
        .and(path(OBJ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&s)
        .await;
    let out = h
        .handle("vibemk_delete_host_tag", &json!({"tag_id": "dc"}))
        .await;
    assert!(text(&out).contains("Repair mode: Disabled\n"));
    let reqs = s.received_requests().await.unwrap();
    assert!(reqs
        .iter()
        .filter(|r| r.method.as_str() == "DELETE")
        .all(|r| r.url.query().is_none()));
    assert_eq!(
        text(&h.handle("vibemk_delete_host_tag", &json!({})).await),
        "❌ **Missing parameter**\n\ntag_id is required"
    );
    assert_eq!(
        text(&h.handle("vibemk_nope", &json!({})).await),
        "❌ **Unknown tool**\n\nTool 'vibemk_nope' is not supported"
    );
}
