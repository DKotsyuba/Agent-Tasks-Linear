//! Opt-in native Linear pilot. Creates a readable disposable project; reports are synthetic Git evidence.
use agent_tasks_linear::{gateway::Gateway, linear::Linear, records::child_id};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use uuid::Uuid;

/// Resumable pilot call journal containing no credentials and never used as production workflow state.
struct Pilot {
    /// Real workflow dispatcher behind the tested MCP transports.
    gateway: Arc<Gateway>,
    /// Owner-selected test team.
    team: String,
    /// Stable run ID used only for request identities, never issue titles.
    run: String,
    /// Local non-secret test evidence path, required explicitly by the operator.
    path: PathBuf,
    /// Completed steps and native results, written after each confirmed response.
    report: Value,
}
impl Pilot {
    /// Resume an explicit report path or initialize its identifiers before the first API mutation.
    fn new() -> Self {
        let key = std::env::var("LINEAR_API_KEY")
            .ok()
            .or_else(|| {
                std::env::var("LINEAR_API_KEY_FILE").ok().map(|p| {
                    std::fs::read_to_string(p)
                        .expect("read API key file")
                        .trim()
                        .to_owned()
                })
            })
            .expect("Set LINEAR_API_KEY or LINEAR_API_KEY_FILE");
        let team = std::env::var("ATL_LIVE_TEAM_ID").expect("Set ATL_LIVE_TEAM_ID");
        let path = PathBuf::from(
            std::env::var("ATL_LIVE_REPORT")
                .expect("Set ATL_LIVE_REPORT to an explicit resumable evidence path"),
        );
        let report = if path.exists() {
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap()
        } else {
            json!({"run":Uuid::new_v4().to_string(),"team":team,"steps":{}})
        };
        assert_eq!(report["team"], team, "Report belongs to a different team");
        let run = report["run"].as_str().unwrap().to_owned();
        std::fs::write(&path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        Self {
            gateway: Gateway::new(Linear::new(Some(key), false).unwrap()).unwrap(),
            team,
            run,
            path,
            report,
        }
    }
    /// Allocate a repeatable v4-format request identity for a named pilot step.
    fn id(&self, key: &str) -> String {
        child_id(&self.run, key)
    }
    /// Execute and journal one confirmed tool call; unknown writes stop and preserve its retry identity.
    async fn call(&mut self, key: &str, tool: &str, mut args: Value) -> Value {
        if self.report["steps"][key].is_object() {
            return self.report["steps"][key].clone();
        }
        if !matches!(tool, "get_context" | "list_items" | "search") {
            args["request_id"] = json!(self.id(key));
            args["actor"] = json!("codex:live-pilot");
        }
        let out = self.gateway.call(tool, args).await;
        assert_eq!(out.status, "ok", "{key} ({tool}): {}", out.data);
        self.report["steps"][key] = out.data.clone();
        std::fs::write(&self.path, serde_json::to_vec_pretty(&self.report).unwrap()).unwrap();
        eprintln!("live step: {key} confirmed");
        out.data
    }
    /// Advance one status through the public transition guard.
    async fn mv(&mut self, key: &str, id: &str, status: &str) {
        self.call(
            key,
            "move_status",
            json!({"id":id,"status":status,"actor_role":"orchestrator"}),
        )
        .await;
    }
    /// Record a synthetic independent review, explicitly identifying the pilot's evidence scope.
    async fn review(&mut self, key: &str, id: &str) {
        self.call(key,"record_review",json!({"id":id,"reviewer":"codex:pilot-reviewer","verdict":"accepted","summary":"Проверены записи и переходы MCP в реальном Linear. Git-артефакты учебные; реальный PR не создавался.","findings":"","artifacts":["https://example.com/mcp-fixtures/review-report"]})).await;
    }
}
/// Real two-module cycle through all sixteen tools, with readable data and no native status provisioning.
#[tokio::test]
#[ignore = "Writes a disposable Linear project; requires explicit test team, credential and report path"]
async fn native_linear_two_module_cycle() {
    let mut p = Pilot::new();
    let project = p.id("project");
    let epic = p.id("epic");
    let modules = [p.id("module_one"), p.id("module_two")];
    let tasks = [p.id("task_one"), p.id("task_two")];
    let seam = p.id("integration");
    p.call("project","create_project",json!({"team_id":p.team,"title":"Проверка базового MCP","description":"Тестовый проект для проверки нового агентского цикла. Ссылки на Git-артефакты — учебные данные.","repository_url":"https://github.com/modelcontextprotocol/rust-sdk"})).await;
    p.call("project_edit","edit_project",json!({"id":project,"description":"Проверка двух модулей, тасок, общего ревью и интеграционной проверки через API Linear. Git-артефакты учебные."})).await;
    p.call("epic","create_epic",json!({"project_id":project,"team_id":p.team,"title":"Пройти агентский цикл в Linear","fields":{"description":"Проверить основную модель MCP на тестовых данных","business_requirements":"Понятные задачи, результаты и история работы","expected_result":"Два завершённых модуля и проверенное взаимодействие","scope":"Только проверка MCP; Git-операции не выполняются","acceptance_criteria":"Нативные статусы, ревью модуля и спайка работают"}})).await;
    for (i, key) in ["module_one", "module_two"].iter().enumerate() {
        p.call(key,"create_module",json!({"project_id":project,"team_id":p.team,"parent_id":epic,"title":if i==0{"Подготовить источник данных"}else{"Обработать данные источника"},"fields":{"description":"Тестовая поставка для проверки MCP","expected_result":"Проверяемый контракт","acceptance_criteria":"Сценарии выполнены","required_contract":if i==0{"Не требуется"}else{"Данные источника"},"provided_contract":if i==0{"Данные источника"}else{"Результат обработки"},"lead":"codex:live-pilot","branch":format!("test/module-{}",i+1),"worktree":format!("/tmp/mcp-pilot/module-{}",i+1)}})).await;
    }
    for (i, key) in ["task_one", "task_two"].iter().enumerate() {
        p.call(key,"create_task",json!({"project_id":project,"team_id":p.team,"parent_id":modules[i],"title":if i==0{"Описать формат входных данных"}else{"Проверить обработку входных данных"},"fields":{"expected_result":"Рабочий результат шага","acceptance_criteria":"Локальная проверка проходит","local_check":"Проверить ожидаемый формат результата","work_type":if i==0{"code"}else{"non_code"}}})).await;
    }
    p.mv("epic_start", &epic, "In Progress").await;
    for i in 0..2 {
        p.mv(&format!("module_start_{i}"), &modules[i], "In Progress")
            .await;
        p.mv(&format!("task_start_{i}"), &tasks[i], "In Progress")
            .await;
        let mut fields = json!({"result":"Тестовый результат шага записан","check_result":"Локальная проверка учебного сценария успешна"});
        fields[if i == 0 { "commit_url" } else { "artifact_url" }] =
            json!("https://example.com/mcp-fixtures/task-result");
        p.call(
            &format!("task_result_{i}"),
            "edit_task",
            json!({"id":tasks[i],"fields":fields}),
        )
        .await;
        p.mv(&format!("task_done_{i}"), &tasks[i], "Done").await;
        p.call(&format!("module_result_{i}"),"edit_module",json!({"id":modules[i],"fields":{"pr_url":"https://example.com/mcp-fixtures/pull-request","result":"Таски модуля завершены","check_result":"Проверены учебные сценарии модуля"}})).await;
        p.mv(&format!("module_review_{i}"), &modules[i], "In Review")
            .await;
        p.review(&format!("review_report_{i}"), &modules[i]).await;
        p.call(&format!("module_merge_{i}"),"edit_module",json!({"id":modules[i],"fields":{"merge_report":"Учебное сообщение агента о слиянии; Git-операции не выполнялись"}})).await;
        p.mv(&format!("module_done_{i}"), &modules[i], "Done").await;
    }
    p.call("integration","create_atomic",json!({"project_id":project,"team_id":p.team,"parent_id":epic,"title":"Проверить взаимодействие модулей","fields":{"work_type":"integration","executor":"codex:live-pilot","expected_result":"Источник и обработчик совместимы","acceptance_criteria":"Сценарий сквозной обработки успешен","local_check":"Проверить прохождение данных по обоим модулям","integration_modules":modules,"scenarios":"Передать результат источника в обработчик","environment":"Учебный интеграционный стенд"}})).await;
    p.mv("integration_start", &seam, "In Progress").await;
    let doc=p.call("integration_doc","save_document",json!({"issue_id":seam,"title":"Результат проверки взаимодействия","content":"Сквозной цикл MCP прошёл реальные записи Linear. Проверены готовность модулей и запись результата спайки. Это тест системы задач; Git-артефакты учебные."})).await;
    p.call("integration_doc_edit","save_document",json!({"id":doc["id"],"content":"Подтверждено через реальный API Linear: обе таски Done, модули прошли ревью, отчёт спайки прикреплён. Git-артефакты учебные."})).await;
    p.call("integration_result","edit_atomic",json!({"id":seam,"fields":{"result":"Совместный учебный сценарий выполнен","check_result":"Оба модуля доступны в проверяемом составе","artifact_url":doc["url"]}})).await;
    p.mv("integration_review", &seam, "In Review").await;
    p.review("integration_report", &seam).await;
    p.mv("integration_done", &seam, "Done").await;
    p.call("epic_result","edit_epic",json!({"id":epic,"fields":{"result":"Два модуля завершены, локальные результаты и общее взаимодействие отражены в Linear."}})).await;
    p.mv("epic_review", &epic, "In Review").await;
    p.review("epic_report", &epic).await;
    p.mv("epic_done", &epic, "Done").await;
    p.call(
        "context_project",
        "get_context",
        json!({"type":"project","id":project}),
    )
    .await;
    p.call(
        "context_document",
        "get_context",
        json!({"type":"document","id":doc["id"]}),
    )
    .await;
    p.call(
        "list_modules",
        "list_items",
        json!({"type":"issue","project_id":project,"kind":"module","status":"Done","first":1}),
    )
    .await;
    p.call(
        "list_projects",
        "list_items",
        json!({"type":"project","first":1}),
    )
    .await;
    p.call(
        "list_documents",
        "list_items",
        json!({"type":"document","project_id":project,"first":1}),
    )
    .await;
    for kind in ["issue", "project", "document"] {
        p.call(
            &format!("search_{kind}"),
            "search",
            json!({"type":kind,"query":"проверка","first":5}),
        )
        .await;
    }
    // Exercise the final write/readback guard on an independent non-code delivery as well.
    let standalone = p.id("standalone_atomic");
    p.call("standalone_atomic", "create_atomic", json!({"project_id":project,"team_id":p.team,
        "title":"Проверить независимый атомик", "fields":{"work_type":"non_code",
        "executor":"codex:live-pilot", "expected_result":"Читаемый отчёт доступен в карточке",
        "acceptance_criteria":"Записанные поля совпадают с ответом Linear", "local_check":"Прочитать результат через новый процесс"}})).await;
    p.mv("standalone_start", &standalone, "In Progress").await;
    p.call("standalone_result", "edit_atomic", json!({"id":standalone,"fields":{
        "result":"Запись подтверждена реальным ответом Linear", "check_result":"Полный ответ совпадает с запрошенными полями", "artifact_url":doc["url"]}})).await;
    p.mv("standalone_review", &standalone, "In Review").await;
    p.review("standalone_report", &standalone).await;
    p.mv("standalone_done", &standalone, "Done").await;
    // A fresh gateway has no knowledge except native Linear data.
    p.gateway = Gateway::new(p.gateway.store.linear.clone()).unwrap();
    for id in [
        &epic,
        &modules[0],
        &modules[1],
        &tasks[0],
        &tasks[1],
        &seam,
        &standalone,
    ] {
        let out = p
            .gateway
            .call("get_context", json!({"type":"issue","id":id}))
            .await;
        assert_eq!(out.status, "ok", "{}", out.data);
        assert_eq!(out.data["issue"]["state"]["name"], "Done");
        assert_eq!(out.data["discrepancies"], json!([]));
    }
    p.report["verified"] = json!(true);
    std::fs::write(&p.path, serde_json::to_vec_pretty(&p.report).unwrap()).unwrap();
    eprintln!(
        "live verified project: {}",
        p.report["steps"]["project"]["project"]["url"]
    );
}
