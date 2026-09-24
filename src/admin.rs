//! Explicit administrative preparation, read-only doctor, and replayable bootstrap plans.

use crate::{
    config::Config,
    linear::Linear,
    model::{Fault, Principal, Result, array, require, text},
    records::Store,
};
use serde_json::{Value, json};
use uuid::Uuid;

/// Produce a non-secret bootstrap plan with every creation UUID reserved before any API call.
pub fn bootstrap_plan(team: &str, name: &str) -> Result<Value> {
    Uuid::parse_str(team).map_err(|_| Fault::new("INVALID_INPUT", "Team ID must be a UUID"))?;
    require(
        !name.trim().is_empty() && name.len() <= 120,
        "INVALID_INPUT",
        "Product name must contain 1–120 bytes",
    )?;
    let mut states = json!({});
    for (name, kind) in [
        ("draft", "backlog"),
        ("ready", "unstarted"),
        ("in_progress", "started"),
        ("review", "started"),
        ("accepted", "completed"),
        ("skipped", "canceled"),
        ("cancelled", "canceled"),
    ] {
        states[name] = json!({"id":Uuid::new_v4().to_string(),"type":kind,"name":format!("AT {}",name.replace('_'," "))});
    }
    let mut labels = json!({});
    for name in [
        "module",
        "task",
        "atomic",
        "control",
        "epic_companion",
        "managed",
    ] {
        labels[name] =
            json!({"id":Uuid::new_v4().to_string(),"name":format!("AT {}",name.replace('_'," "))});
    }
    let mut statuses = json!({});
    for (position, (name, kind)) in [
        ("draft", "backlog"),
        ("ready", "planned"),
        ("in_progress", "started"),
        ("review", "started"),
        ("accepted", "completed"),
        ("skipped", "canceled"),
        ("cancelled", "canceled"),
    ]
    .into_iter()
    .enumerate()
    {
        statuses[name] = json!({"id":Uuid::new_v4().to_string(),"type":kind,"name":format!("AT {name}"),"position":position});
    }
    Ok(
        json!({"schema_version":1,"name":name,"team_id":team,"initiative_id":Uuid::new_v4().to_string(),"general_project_id":Uuid::new_v4().to_string(),"control_issue_id":Uuid::new_v4().to_string(),"initiative_link_id":Uuid::new_v4().to_string(),"config_record_id":Uuid::new_v4().to_string(),"identity_record_id":Uuid::new_v4().to_string(),"head_record_id":Uuid::new_v4().to_string(),"issue_states":states,"labels":labels,"project_statuses":statuses}),
    )
}

/// Show configured/live status and readable native team IDs without modifying Linear.
pub async fn doctor(linear: &Linear, config: &Config) -> Result<Value> {
    if !linear.configured() {
        return Ok(
            json!({"configured":true,"api_token_present":false,"live_verified":false,"next":"Set LINEAR_API_KEY or LINEAR_OAUTH_TOKEN, then run doctor again","listen":config.listen.to_string(),"bindings":config.bindings.iter().map(|b|json!({"name":b.name,"role":b.principal.role,"principal_id":b.principal.id})).collect::<Vec<_>>()}),
        );
    }
    let viewer = linear.call("QViewer", json!({})).await?;
    let mut teams = vec![];
    let mut after = Value::Null;
    let mut complete = false;
    for _ in 0..20 {
        let data = linear
            .call("QTeams", json!({"first":50,"after":after}))
            .await?;
        for team in array(&data["teams"], "nodes") {
            let detail = linear.object("QTeam", "team", text(team, "id")?).await?;
            teams.push(json!({"id":team["id"],"name":team["name"],"unsafe_auto_close":detail["autoCloseParentIssues"]==true||detail["autoCloseChildIssues"]==true}));
        }
        if data["teams"]["pageInfo"]["hasNextPage"] == false {
            complete = true;
            break;
        }
        let next = data["teams"]["pageInfo"]["endCursor"].clone();
        require(
            next.is_string() && next != after,
            "INCOMPLETE_DATA",
            "Team pagination did not advance",
        )?;
        after = next;
    }
    require(complete, "INCOMPLETE_DATA", "Team page budget exhausted")?;
    Ok(
        json!({"api_token_present":true,"viewer":viewer["viewer"],"teams":teams,"read_access_verified":true,"workflow_live_verified":false,"next":"Use a dedicated team with auto-close disabled; generate and inspect a bootstrap plan"}),
    )
}

/// Apply a reviewed plan using reserved IDs; existing mismatched objects cause a safe refusal.
pub async fn bootstrap(store: &Store, owner: &Principal, plan: &Value) -> Result<Value> {
    require(
        owner.role == crate::model::Role::Owner,
        "UNAUTHORIZED",
        "Bootstrap requires an owner binding",
    )?;
    let team = text(plan, "team_id")?;
    let name = text(plan, "name")?;
    let product = text(plan, "control_issue_id")?;
    let viewer = store.linear.call("QViewer", json!({})).await?;
    let native = store.linear.object("QTeam", "team", team).await?;
    require(
        native["autoCloseParentIssues"] != true && native["autoCloseChildIssues"] != true,
        "UNSAFE_AUTOMATION",
        "Disable auto-close in this dedicated team before applying the plan",
    )?;
    require(
        native["states"]["pageInfo"]["hasNextPage"] == false,
        "INCOMPLETE_DATA",
        "Team has more than 50 states; choose a dedicated team",
    )?;
    for state in plan["issue_states"]
        .as_object()
        .ok_or_else(|| Fault::new("INVALID_INPUT", "Missing planned states"))?
        .values()
    {
        if let Some(found) = array(&native["states"], "nodes")
            .iter()
            .find(|r| r["id"] == state["id"])
        {
            require(
                found["name"] == state["name"] && found["type"] == state["type"],
                "STRUCTURE_DRIFT",
                "Reserved state differs from the plan",
            )?;
        } else {
            store.linear.call("MCreateWorkflowState",json!({"input":{"id":state["id"],"name":state["name"],"type":state["type"],"color":"#5E6AD2","teamId":team}})).await?;
            let created = store
                .linear
                .object("QWorkflowState", "workflowState", text(state, "id")?)
                .await?;
            require(
                created["name"] == state["name"] && created["type"] == state["type"],
                "LINEAR_PARTIAL_ERROR",
                "Workflow state read-back differs from the plan",
            )?;
        }
    }
    for label in plan["labels"]
        .as_object()
        .ok_or_else(|| Fault::new("INVALID_INPUT", "Missing planned labels"))?
        .values()
    {
        ensure(
            &store.linear,
            "QIssueLabel",
            "issueLabel",
            text(label, "id")?,
            "MCreateIssueLabel",
            json!({"id":label["id"],"name":label["name"],"color":"#5E6AD2","teamId":team}),
            "name",
            &label["name"],
        )
        .await?;
    }
    for status in plan["project_statuses"]
        .as_object()
        .ok_or_else(|| Fault::new("INVALID_INPUT", "Missing project statuses"))?
        .values()
    {
        ensure(&store.linear,"QProjectStatus","projectStatus",text(status,"id")?,"MCreateProjectStatus",json!({"id":status["id"],"name":status["name"],"type":status["type"],"color":"#5E6AD2","position":status["position"]}),"name",&status["name"]).await?;
    }
    ensure(
        &store.linear,
        "QInitiative",
        "initiative",
        text(plan, "initiative_id")?,
        "MCreateInitiative",
        json!({"id":plan["initiative_id"],"name":name}),
        "name",
        &json!(name),
    )
    .await?;
    let general_name = format!("{name} · General");
    ensure(
        &store.linear,
        "QProject",
        "project",
        text(plan, "general_project_id")?,
        "MCreateProject",
        json!({"id":plan["general_project_id"],"name":general_name,"teamIds":[team]}),
        "name",
        &json!(general_name),
    )
    .await?;
    ensure(&store.linear,"QInitiativeLink","initiativeToProject",text(plan,"initiative_link_id")?,"MLinkProjectToInitiative",json!({"id":plan["initiative_link_id"],"initiativeId":plan["initiative_id"],"projectId":plan["general_project_id"]}),"id",&plan["initiative_link_id"]).await?;
    let title = format!("[AT control] {name}");
    ensure(&store.linear,"QIssue","issue",product,"MCreateIssue",json!({"id":product,"title":title,"description":"Agent-Tasks-Linear product control. Workflow metadata and receipts are attached here.","teamId":team,"projectId":plan["general_project_id"],"stateId":plan["issue_states"]["draft"]["id"],"labelIds":[plan["labels"]["control"]["id"],plan["labels"]["managed"]["id"]]}),"title",&json!(title)).await?;
    let issue = store.linear.object("QIssue", "issue", product).await?;
    let base = text(&issue, "url")?;
    let existing = store.linear.attachments(product).await?;
    if existing.iter().any(|a| {
        a["metadata"]["at_product_id"] == product && a["metadata"]["at_kind"] == "product_config"
    }) {
        let snapshot = store.snapshot(product).await?;
        require(
            snapshot.config.payload["general_project_id"] == plan["general_project_id"],
            "STRUCTURE_DRIFT",
            "Product already exists with another mapping",
        )?;
        return Ok(json!({"product_id":product,"url":base,"status":"already_initialized"}));
    }
    let states = plan["issue_states"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v["id"].clone()))
        .collect::<serde_json::Map<_, _>>();
    let statuses = plan["project_statuses"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v["id"].clone()))
        .collect::<serde_json::Map<_, _>>();
    let labels = plan["labels"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(k, _)| k.as_str() != "managed")
        .map(|(k, v)| (k.clone(), v["id"].clone()))
        .collect::<serde_json::Map<_, _>>();
    let config=store.signer.record(owner,product,product,"product_config",json!({"initiative_id":plan["initiative_id"],"general_project_id":plan["general_project_id"],"control_issue_id":product,"record_base_url":base,"team_ids":[team],"owner_linear_user_ids":[viewer["viewer"]["id"]],"epoch":1,"policy_version":1,"issue_state_ids":states,"project_status_ids":statuses,"kind_label_ids":labels,"managed_label_id":plan["labels"]["managed"]["id"],"pending_operation_key":null,"product_state":"active","policy":{"single_active_writer":true,"independent_module_review":true,"independent_epic_review":true,"atomic_review_default":"all","integration_uses_owner_pat":true,"allow_root_attributed_owner_decisions":false,"nested_atomic_verified":false}}),Some(text(plan,"config_record_id")?.into()))?;
    let identity=store.signer.record(owner,product,product,"identity",json!({"kind":"product","primary_parent_id":null,"native_issue_id":product,"native_project_id":plan["general_project_id"],"native_parent_id":null,"record_base_url":base,"mandatory":true}),Some(text(plan,"identity_record_id")?.into()))?;
    let head=store.signer.record(owner,product,product,"work_head",json!({"state":"draft","children":[],"assignment_ids":[],"native_state_id":plan["issue_states"]["draft"]["id"]}),Some(text(plan,"head_record_id")?.into()))?;
    store.put(&identity, base).await?;
    store.put(&head, base).await?;
    store.put(&config, base).await?;
    store.snapshot(product).await?;
    Ok(
        json!({"product_id":product,"url":base,"general_project_id":plan["general_project_id"],"status":"initialized","live_workflow_verified":false}),
    )
}

/// Create a reserved object only after a definite not-found read; never treat network errors as absence.
#[expect(
    clippy::too_many_arguments,
    reason = "Keeps the explicit read/create/read-back contract together without a one-use request type"
)]
async fn ensure(
    api: &Linear,
    query: &str,
    field: &str,
    id: &str,
    mutation: &str,
    input: Value,
    check: &str,
    expected: &Value,
) -> Result<()> {
    match api.object(query, field, id).await {
        Ok(existing) => require(
            &existing[check] == expected,
            "STRUCTURE_DRIFT",
            "Reserved bootstrap object differs from its plan",
        ),
        Err(error) if error.code == "RECORD_MISSING" => {
            api.call(mutation, json!({"input":input})).await?;
            let created = api.object(query, field, id).await?;
            require(
                &created[check] == expected,
                "LINEAR_PARTIAL_ERROR",
                "Bootstrap read-back failed",
            )
        }
        Err(error) => Err(error),
    }
}
