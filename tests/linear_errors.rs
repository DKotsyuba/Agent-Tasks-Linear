//! Regression coverage for observed Linear error envelopes and unconfirmed mutation results.

use agent_tasks_linear::linear::Linear;
use axum::{Json, Router, routing::post};
use serde_json::{Value, json};

/// The live INPUT_ERROR shape identifies absence only with its exact entity-not-found message.
#[tokio::test]
async fn missing_entity_is_distinct_from_invalid_input_and_uncertain_writes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let app=Router::new().route("/",post(|Json(request):Json<Value>|async move{
        let body=if request["operationName"]=="MCreateIssue"{json!({"data":{}})}else{
            let message=if request["variables"]["id"]=="missing"{"Entity not found: IssueLabel"}else{"Invalid id"};
            json!({"data":null,"errors":[{"message":message,"extensions":{"code":"INPUT_ERROR","type":"invalid input"}}]})
        };Json(body)
    }));
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let api = Linear::mock(&endpoint).unwrap();
    let missing = api
        .call("QIssueLabel", json!({"id":"missing"}))
        .await
        .unwrap_err();
    assert_eq!(missing.code, "RECORD_MISSING");
    assert!(!missing.uncertain);
    let invalid = api
        .call("QIssueLabel", json!({"id":"invalid"}))
        .await
        .unwrap_err();
    assert_eq!(invalid.code, "LINEAR_PARTIAL_ERROR");
    let write = api
        .call("MCreateIssue", json!({"input":{}}))
        .await
        .unwrap_err();
    assert_eq!(write.code, "LINEAR_PARTIAL_ERROR");
    assert!(write.uncertain);
    task.abort();
}
