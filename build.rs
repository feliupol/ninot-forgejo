//! Generates the operation table from the pinned Forgejo Swagger description, through
//! [`ninot_common::spec`], the version the description names, and the Rust models of supported
//! request and response bodies through [`typify`].
//!
//! The models come from the definitions [`ROOTS`] names and everything they refer to, after
//! [`spec/overrides.json`](OVERRIDES) has corrected what the description gets wrong or leaves
//! out.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::PathBuf;

use schemars::schema::Schema;
use serde_json::{Map, Value};
use typify::TypeSpaceSettings;

const SPEC: &str = "spec/forgejo.v1.json";
const OVERRIDES: &str = "spec/overrides.json";

/// The definitions the fake answers a response with, from which the rest of the models are
/// reached by `$ref`. Every root is used by a supported request, response, or webhook payload.
const ROOTS: &[&str] = &[
    // Request bodies accepted by the operations this fake handles.
    "CreateBranchRepoOption",
    "CreateFileOptions",
    "CreateHookOption",
    "CreateIssueCommentOption",
    "CreateIssueOption",
    "CreateLabelOption",
    "CreateOrUpdateSecretOption",
    "CreatePullRequestOption",
    "CreateRepoOption",
    "CreateStatusOption",
    "DispatchWorkflowOption",
    "DispatchWorkflowRun",
    "EditHookOption",
    "EditIssueCommentOption",
    "EditIssueOption",
    "EditLabelOption",
    "EditPullRequestOption",
    "EditRepoOption",
    "IssueLabelsOption",
    "MergePullRequestOption",
    "UpdateFileOptions",
    // Response used to list repository action-secret names.
    "Secret",
    // An error body, which the server fills the API URL into.
    "APIError",
    // A branch listing, and the file contents beside it.
    "Branch",
    "ContentsResponse",
    "FileResponse",
    "ActionTaskResponse",
    "ListActionRunResponse",
    "ActionRunJob",
    // The version anyone may read.
    "ServerVersion",
    // The topics of a repository, which the fake models none of.
    "TopicName",
    // A user, which every repository, issue, comment and payload names one of.
    "User",
    // A repository, with its permissions.
    "Repository",
    // The issues, labels and comments hanging off a repository.
    "Issue",
    // Pull-request timeline events.
    "TimelineComment",
    "Label",
    "Comment",
    // The pull requests of a repository, with their commits and their changed files.
    "PullRequest",
    "Commit",
    "ChangedFile",
    "CommitStatus",
    "CombinedStatus",
    "Compare",
    // The webhooks of a repository.
    "Hook",
    // The payload of the one event the fake sends.
    "PushPayload",
];

fn main() {
    println!("cargo::rerun-if-changed={SPEC}");
    println!("cargo::rerun-if-changed={OVERRIDES}");
    let spec: Value = serde_json::from_slice(&fs::read(SPEC).expect("cannot read the pinned spec"))
        .expect("the pinned spec is not valid JSON");
    let version = spec["info"]["version"]
        .as_str()
        .expect("the pinned spec names no version");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(
        out.join("operations.rs"),
        ninot_common::spec::operations(&spec),
    )
    .expect("cannot write the generated operations");
    fs::write(
        out.join("version.rs"),
        format!("pub const VERSION: &str = {version:?};\n"),
    )
    .expect("cannot write the version");
    fs::write(out.join("forgejo.rs"), models(&spec)).expect("cannot write the generated types");
}

/// The Rust models of supported request and response bodies and webhook payloads, generated
/// from the pinned spec's definitions with [`OVERRIDES`] applied.
fn models(spec: &Value) -> String {
    let mut definitions = spec["definitions"]
        .as_object()
        .expect("the pinned spec has no definitions")
        .clone();
    let overrides: Value =
        serde_json::from_slice(&fs::read(OVERRIDES).expect("cannot read the overrides"))
            .expect("the overrides are not valid JSON");
    if let Some(corrections) = overrides["definitions"].as_object() {
        correct(corrections, &mut definitions);
    }
    if let Some(additions) = overrides["additions"].as_object() {
        add(additions, &mut definitions);
    }

    let mut wanted = BTreeSet::new();
    for root in ROOTS {
        wanted.extend(named(&definitions, root));
    }
    let schemas: Vec<(String, Schema)> = wanted
        .iter()
        .map(|name| {
            let schema = serde_json::from_value::<Schema>(definitions[name].clone())
                .unwrap_or_else(|error| panic!("{name} is no schema: {error}"));
            (name.clone(), schema)
        })
        .collect();
    let mut space = typify::TypeSpace::new(&TypeSpaceSettings::default());
    space
        .add_ref_types(schemas)
        .expect("cannot convert the definitions to types");
    let generated = space.to_stream();
    let file = syn::parse2(generated).expect("the generated types are not valid Rust");
    format!(
        "// Generated by build.rs from {SPEC} with {OVERRIDES} through typify. Do not edit.\n\
         // Every type below is a model of one of those definitions; edit {OVERRIDES} to change\n\
         // one, never this file.\n\n{}",
        prettyplease::unparse(&file)
    )
}

/// The name of `root` and of every definition it reaches through a `$ref`, so that a generated
/// type names no other type the fake does not have.
fn named(definitions: &Map<String, Value>, root: &str) -> Vec<String> {
    let mut found = BTreeSet::from([root.to_owned()]);
    let mut pending = vec![root.to_owned()];
    while let Some(name) = pending.pop() {
        for reference in references(&definitions[&name]) {
            if found.insert(reference.clone()) {
                pending.push(reference);
            }
        }
    }
    found.into_iter().collect()
}

/// The definitions `definition` refers to, wherever a `$ref` sits in it.
fn references(definition: &Value) -> BTreeSet<String> {
    fn walk(value: &Value, into: &mut BTreeSet<String>) {
        match value {
            Value::Object(object) => {
                for (name, value) in object {
                    if name == "$ref" {
                        let reference = value
                            .as_str()
                            .and_then(|reference| reference.rsplit('/').next())
                            .unwrap_or_else(|| panic!("{value} names no definition"));
                        into.insert(reference.to_owned());
                    } else {
                        walk(value, into);
                    }
                }
            }
            Value::Array(values) => values.iter().for_each(|value| walk(value, into)),
            _ => {}
        }
    }
    let mut into = BTreeSet::new();
    walk(definition, &mut into);
    into
}

/// Applies `corrections` to `definitions`, each of which says what it corrects and why. The
/// added properties go in first, so that a correction may declare one of them as sent.
fn correct(corrections: &Map<String, Value>, definitions: &mut Map<String, Value>) {
    for (name, correction) in corrections {
        reason(name, correction);
        let definition = definitions
            .get_mut(name)
            .unwrap_or_else(|| panic!("the spec has no definition {name}"));
        correct_one(name, correction, definition);
    }
}

/// Corrects one definition, `name` being there for the messages.
fn correct_one(name: &str, correction: &Value, definition: &mut Value) {
    if let Some(properties) = correction["properties"].as_object() {
        let Some(declared) = definition["properties"].as_object_mut() else {
            panic!("{name} declares no properties to correct");
        };
        for (property, schema) in properties {
            declared.insert(property.clone(), schema.clone());
        }
    }
    let mut required = definition["required"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for property in listed(correction, "required") {
        let _ = property_of(definition, &property);
        required.push(property.into());
    }
    if !required.is_empty() {
        required.sort_by_key(|property| property.as_str().unwrap_or_default().to_owned());
        required.dedup();
        let _ = definition
            .as_object_mut()
            .expect("the definition is no object")
            .insert("required".to_owned(), Value::Array(required));
    }
    for property in listed(correction, "nullable") {
        let schema = property_of(definition, &property);
        *schema = nullable(schema);
    }
}

/// Adds the definitions the pinned spec does not describe but the fake sends anyway, each
/// corrected the way a correction of the spec's own definitions is.
fn add(additions: &Map<String, Value>, definitions: &mut Map<String, Value>) {
    for (name, addition) in additions {
        reason(name, addition);
        let schema = addition
            .get("schema")
            .unwrap_or_else(|| panic!("{name} adds no schema"));
        let mut definition = schema.clone();
        correct_one(name, addition, &mut definition);
        let _ = definitions.insert(name.clone(), definition);
    }
}

/// The schema of a property of `definition`, which the definition must declare.
fn property_of<'a>(definition: &'a mut Value, property: &str) -> &'a mut Value {
    let declared = definition["properties"]
        .as_object_mut()
        .unwrap_or_else(|| panic!("the definition declares no properties"));
    if !declared.contains_key(property) {
        panic!("the definition has no property {property}");
    }
    declared
        .get_mut(property)
        .expect("the property is declared")
}

/// `schema` as a property that is always sent and may hold nothing, which typify makes an
/// `Option` serde renders as `null` rather than one it leaves out.
fn nullable(schema: &Value) -> Value {
    let Some(declared) = schema.as_object() else {
        panic!("{schema} is no schema to make nullable");
    };
    let mut nullable = declared.clone();
    match declared.get("$ref").and_then(Value::as_str) {
        Some(reference) => {
            nullable.remove("$ref");
            let _ = nullable.insert(
                "anyOf".to_owned(),
                serde_json::json!([{ "$ref": reference }, { "type": "null" }]),
            );
        }
        None => {
            let named = declared.get("type").cloned().unwrap_or(Value::Null);
            assert!(
                named.is_string() || named.is_array(),
                "{schema} names no type to make nullable"
            );
            let types = match named.as_array() {
                Some(types) => Value::Array(types.clone()),
                None => Value::Array(vec![named, Value::String("null".to_owned())]),
            };
            let _ = nullable.insert("type".to_owned(), types);
        }
    }
    Value::Object(nullable)
}

/// The properties `correction` lists under `key`.
fn listed(correction: &Value, key: &str) -> Vec<String> {
    let Some(properties) = correction[key].as_array() else {
        return Vec::new();
    };
    properties
        .iter()
        .map(|property| {
            property
                .as_str()
                .unwrap_or_else(|| panic!("{correction} lists no property under {key}"))
                .to_owned()
        })
        .collect()
}

/// Why `correction` is there, which every override says and none may leave out.
fn reason(name: &str, correction: &Value) {
    assert!(
        correction["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "the override of {name} says no reason"
    );
}
