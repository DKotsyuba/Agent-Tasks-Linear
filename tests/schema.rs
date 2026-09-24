//! Static checks against the pinned official Linear schema, without claiming live API validation.

use agent_tasks_linear::{admin, linear::OPERATIONS};
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
    assert!(count >= 33);
}
/// Every generated project status supplies required ordering and both retirement projections.
#[test]
fn bootstrap_status_plan_has_required_position_and_skipped_projection() {
    let plan = admin::bootstrap_plan("10000000-0000-4000-8000-000000000001", "Fixture").unwrap();
    for status in plan["project_statuses"].as_object().unwrap().values() {
        assert!(status["position"].is_number());
    }
    assert_eq!(plan["project_statuses"]["skipped"]["type"], "canceled");
}
