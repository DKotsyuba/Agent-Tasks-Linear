//! Static checks against the pinned official Linear schema, without claiming live API validation.

use agent_tasks_linear::linear::OPERATIONS;
use graphql_parser::{query as q, schema as s};
use std::collections::BTreeMap;

/// Extract the named type below any GraphQL list/non-null wrappers.
fn named<'a, 'b>(value: &'a s::Type<'b, String>) -> &'a str {
    match value {
        s::Type::NamedType(name) => name,
        s::Type::ListType(inner) | s::Type::NonNullType(inner) => named(inner),
    }
}
/// Compare a variable declaration with an argument, permitting a stricter non-null variable.
fn compatible(actual: &q::Type<'_, String>, expected: &s::Type<'_, String>) -> bool {
    match (actual, expected) {
        (q::Type::NonNullType(a), s::Type::NonNullType(e)) => compatible(a, e),
        (q::Type::NonNullType(a), e) => compatible(a, e),
        (_, s::Type::NonNullType(_)) => false,
        (q::Type::ListType(a), s::Type::ListType(e)) => compatible(a, e),
        (q::Type::NamedType(a), s::Type::NamedType(e)) => a == e,
        _ => false,
    }
}
/// Check every selected field/argument recursively against actual SDL object definitions.
fn selections<'a>(
    selection: &q::SelectionSet<'a, String>,
    parent: &str,
    objects: &BTreeMap<String, s::ObjectType<'a, String>>,
    variables: &[q::VariableDefinition<'a, String>],
) {
    let object = objects
        .get(parent)
        .unwrap_or_else(|| panic!("missing GraphQL object {parent}"));
    for selected in &selection.items {
        let q::Selection::Field(field) = selected else {
            panic!("unexpected fragment in static operations")
        };
        let definition = object
            .fields
            .iter()
            .find(|f| f.name == field.name)
            .unwrap_or_else(|| panic!("{parent}.{} is absent from schema", field.name));
        for (name, value) in &field.arguments {
            let argument = definition
                .arguments
                .iter()
                .find(|a| &a.name == name)
                .unwrap_or_else(|| panic!("unknown argument {parent}.{}({name})", field.name));
            if let q::Value::Variable(name) = value {
                let variable = variables.iter().find(|v| &v.name == name).unwrap();
                assert!(
                    compatible(&variable.var_type, &argument.value_type),
                    "incompatible variable {name} for {}",
                    field.name
                );
            }
        }
        for argument in &definition.arguments {
            if matches!(argument.value_type, s::Type::NonNullType(_))
                && argument.default_value.is_none()
            {
                assert!(
                    field
                        .arguments
                        .iter()
                        .any(|(name, _)| name == &argument.name),
                    "required {}.{}({}) missing",
                    parent,
                    field.name,
                    argument.name
                );
            }
        }
        if !field.selection_set.items.is_empty() {
            selections(
                &field.selection_set,
                named(&definition.field_type),
                objects,
                variables,
            );
        }
    }
}
/// All declared operations resolve in the pinned public schema, including required arguments.
#[test]
fn static_graphql_operations_match_official_snapshot() {
    let schema = s::parse_schema::<String>(include_str!("../schemas/linear.graphql")).unwrap();
    let objects = schema
        .definitions
        .into_iter()
        .filter_map(|d| match d {
            s::Definition::TypeDefinition(s::TypeDefinition::Object(object)) => {
                Some((object.name.clone(), object))
            }
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let query = q::parse_query::<String>(OPERATIONS).unwrap();
    let mut count = 0;
    for definition in query.definitions {
        if let q::Definition::Operation(operation) = definition {
            match operation {
                q::OperationDefinition::Query(query) => selections(
                    &query.selection_set,
                    "Query",
                    &objects,
                    &query.variable_definitions,
                ),
                q::OperationDefinition::Mutation(mutation) => selections(
                    &mutation.selection_set,
                    "Mutation",
                    &objects,
                    &mutation.variable_definitions,
                ),
                _ => panic!("unsupported operation"),
            };
            count += 1;
        }
    }
    assert!(count >= 25);
}

/// The stable public tool identity: exactly these 22 names, in discovery order.
#[test]
fn catalog_pins_all_22_tool_names() {
    let catalog = agent_tasks_linear::catalog::Catalog::new().unwrap();
    let names: Vec<&str> = catalog
        .tools
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "create_project",
            "edit_project",
            "create_epic",
            "edit_epic",
            "create_module",
            "edit_module",
            "create_task",
            "edit_task",
            "create_atomic",
            "edit_atomic",
            "get_context",
            "get_overview",
            "list_items",
            "search",
            "save_document",
            "move_status",
            "record_review",
            "record_commits",
            "add_comment",
            "get_comment",
            "resolve_comment",
            "save_project_update",
        ]
    );
}

/// Every D2 input-contract example validates or fails exactly as marked, against the
/// embedded catalogue, so schema widening never drifts from the published examples.
#[test]
fn catalog_examples_match_the_d2_input_contract() {
    let catalog = agent_tasks_linear::catalog::Catalog::new().unwrap();
    let examples: serde_json::Value =
        serde_json::from_str(include_str!("../schemas/examples.json")).unwrap();
    let cases = examples["cases"].as_array().unwrap();
    assert!(cases.len() >= 20, "contract example set shrank");
    for case in cases {
        let tool = case["tool"].as_str().unwrap();
        let note = case["note"].as_str().unwrap();
        let expected_valid = case["valid"].as_bool().unwrap();
        let outcome = catalog.validate(tool, &case["arguments"]);
        assert_eq!(
            outcome.is_ok(),
            expected_valid,
            "{tool} ({note}): {outcome:?}"
        );
    }
}

/// Exactly the two canonical role skills ship, each a directory-named, versioned SKILL.md.
#[test]
fn exactly_two_role_skills_ship() {
    let skills = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("skills");
    let mut dirs: Vec<String> = std::fs::read_dir(&skills)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort();
    assert_eq!(
        dirs,
        vec![
            "agent-tasks-linear-module-lead",
            "agent-tasks-linear-orchestrator"
        ]
    );
    for dir in &dirs {
        let md = std::fs::read_to_string(skills.join(dir).join("SKILL.md")).unwrap();
        assert!(md.starts_with("---\n"), "{dir}: missing frontmatter");
        assert!(
            md.contains(&format!("name: {dir}\n")),
            "{dir}: name mismatch"
        );
        assert!(md.contains("  version: 1"), "{dir}: missing version");
    }
}
