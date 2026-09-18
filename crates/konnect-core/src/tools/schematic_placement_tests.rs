//! Placement acceptance for #383, using the KiCad-authored complex-hierarchy demo.

use super::*;
use konnect_schematic_editor::{sexp::SexpNode, Schematic};
use serde_json::json;
use std::path::{Path, PathBuf};

const PATH_A: &str = "/5b9623a5-6d01-41fc-9865-e1bc779418c8/00000000-0000-0000-0000-00004b3a1333";
const PATH_B: &str = "/5b9623a5-6d01-41fc-9865-e1bc779418c8/00000000-0000-0000-0000-00004b3a13a4";
const PLACERS: [&str; 3] = [
    "add_schematic_component",
    "batch_place_components",
    "add_power_symbol",
];

/// The sample sizes the stale-target refusal promises its callers, written as
/// literals. Reading the implementation's own constants would let a production
/// change move the oracle with it.
const PROMISED_IDENTITY_SAMPLE: usize = 3;
const PROMISED_DIAGNOSIS_SAMPLE: usize = 5;

fn fixture(reused: bool) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let (root, child) = schematic_target_tests::native_deep_project(directory.path());
    let mut schematic = Schematic::load(&child).unwrap();
    if !reused {
        // Derive a unique-child case by removing one native sheet and its saved
        // per-symbol entries. Do not synthesize the remaining instance metadata.
        let mut parent = Schematic::load(&root).unwrap();
        parent
            .sheets
            .remove_by_uuid("00000000-0000-0000-0000-00004b3a13a4")
            .unwrap();
        parent.overwrite().unwrap();
        for symbol in schematic.symbols.iter_mut() {
            let project = symbol
                .raw_sub_nodes
                .iter_mut()
                .find(|node| node.tag() == Some("instances"))
                .unwrap()
                .find_mut("project")
                .unwrap();
            let SexpNode::List(children) = project else {
                unreachable!()
            };
            children.retain(|node| node.tag() != Some("path") || node.value() != Some(PATH_B));
        }
    }
    // Alias the demo's embedded GND definition under the power tool's library
    // name. Geometry and properties remain KiCad-authored; no host library is needed.
    let library = schematic
        .raw_other
        .iter_mut()
        .find(|node| node.tag() == Some("lib_symbols"))
        .unwrap();
    // Unit validation also consults the library source. Export the native
    // embedded definitions into a temporary project library, keeping their
    // unit sub-symbols intact and only stripping the library namespace.
    let mut exported = library
        .find_all("symbol")
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    for entry in &mut exported {
        let name = entry
            .value()
            .unwrap()
            .split(':')
            .next_back()
            .unwrap()
            .to_string();
        let SexpNode::List(fields) = entry else {
            unreachable!()
        };
        fields[1] = SexpNode::Str(name);
    }
    let mut library_nodes = vec![
        konnect_schematic_editor::sexp::atom("kicad_symbol_lib"),
        konnect_schematic_editor::sexp::tagged(
            "version",
            vec![konnect_schematic_editor::sexp::atom("20241209")],
        ),
        konnect_schematic_editor::sexp::tagged(
            "generator",
            vec![SexpNode::Str("kicad_symbol_editor".to_string())],
        ),
    ];
    library_nodes.extend(exported);
    std::fs::write(
        child.parent().unwrap().join("fixture.kicad_sym"),
        konnect_schematic_editor::sexp::writer::write(&SexpNode::List(library_nodes)),
    )
    .unwrap();
    std::fs::write(child.parent().unwrap().join("sym-lib-table"),
        r#"(sym_lib_table (lib (name "complex_hierarchy") (type "KiCad") (uri "${KIPRJMOD}/fixture.kicad_sym") (options "") (descr "KiCad demo definitions")))"#).unwrap();
    let mut power = library
        .find_all("symbol")
        .into_iter()
        .find(|symbol| symbol.value() == Some("complex_hierarchy:GND"))
        .unwrap()
        .clone();
    let SexpNode::List(power_children) = &mut power else {
        unreachable!()
    };
    power_children[1] = SexpNode::Str("power:GND".to_string());
    let SexpNode::List(library_children) = library else {
        unreachable!()
    };
    library_children.push(power);
    schematic.overwrite().unwrap();
    (directory, root, child)
}

async fn place(name: &str, child: &Path) -> CallToolResult {
    let component = json!({
        "lib_id": "complex_hierarchy:LM358N", "reference": "U999",
        "value": "placement readback", "x": 100.1, "y": 80.2,
        "rotation": 90.0, "unit": 2
    });
    let mut args = match name {
        "add_schematic_component" => component,
        "batch_place_components" => json!({"components": [
            component,
            {"lib_id": "complex_hierarchy:R", "reference": "R999", "x": 110.1, "y": 80.2}
        ]}),
        "add_power_symbol" => json!({"power_net": "GND", "x": 100.1, "y": 80.2, "rotation": 90.0}),
        _ => unreachable!(),
    };
    args["schematic"] = json!(child.display().to_string());
    let context = Arc::new(ToolContext::new(
        ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: String::new(),
            ipc_address: String::new(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: false,
            eager_toolsets: false,
        },
        Arc::new(ToolRouter::new()),
    ));
    let mut tools = sch_components::tools();
    tools.extend(sch_batch::tools());
    tools.extend(sch_wiring::tools());
    let tool = tools.iter().find(|tool| tool.name == name).unwrap();
    (tool.handler)(&args, context).await.unwrap()
}

async fn add_native_component(child: &Path, component: Value) -> CallToolResult {
    let mut args = component;
    args["schematic"] = json!(child.display().to_string());
    let context = Arc::new(ToolContext::new(
        ServerConfig::default(),
        Arc::new(ToolRouter::new()),
    ));
    let tool = sch_components::tools()
        .into_iter()
        .find(|tool| tool.name == "add_schematic_component")
        .unwrap();
    (tool.handler)(&args, context).await.unwrap()
}

fn body(result: &CallToolResult) -> Value {
    let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
        panic!("expected text result")
    };
    serde_json::from_str(text).unwrap()
}

#[tokio::test]
async fn native_placement_preserves_unique_and_reused_paths_with_committed_readback() {
    for reused in [false, true] {
        for name in PLACERS {
            let (_directory, root, child) = fixture(reused);
            let root_before = std::fs::read(&root).unwrap();
            let project_before = std::fs::read(root.with_extension("kicad_pro")).unwrap();
            let existing = Schematic::load(&child).unwrap().symbols.into_vec();
            let result = place(name, &child).await;
            assert!(!result.is_error, "{name}, reused={reused}: {result:?}");
            let response = body(&result);
            let placed = if name == "batch_place_components" {
                assert_eq!(response["placed_count"], 2);
                assert_eq!(response["errors"], json!([]));
                response["placed"].as_array().unwrap().clone()
            } else {
                vec![response]
            };
            let committed = Schematic::load(&child).unwrap();
            assert_eq!(committed.symbols.len(), existing.len() + placed.len());
            let expected = if reused {
                vec![PATH_A, PATH_B]
            } else {
                vec![PATH_A]
            };
            for entry in placed {
                let symbol = committed
                    .symbols
                    .iter()
                    .find(|symbol| Some(symbol.uuid.as_str()) == entry["uuid"].as_str())
                    .unwrap();
                let mut identities = symbol.instance_paths();
                identities.sort();
                let observed_project = identities[0].0.clone();
                assert_eq!(
                    identities,
                    expected
                        .iter()
                        .map(|path| ("complex_hierarchy".to_string(), path.to_string()))
                        .collect::<Vec<_>>()
                );
                assert_eq!(entry["instance_paths"], json!(expected));
                assert_eq!(
                    entry["schematic"],
                    committed.filepath().display().to_string()
                );
                assert_eq!(entry["project"], observed_project);
                assert_eq!(entry["added"], symbol.lib_id);
                assert_eq!(entry["reference"], symbol.reference().unwrap());
                assert_eq!(entry["value"], symbol.value_str().unwrap());
                assert_eq!(entry["x"].as_f64(), Some(symbol.at.x));
                assert_eq!(entry["y"].as_f64(), Some(symbol.at.y));
                assert_eq!(entry["rotation"].as_f64(), symbol.at.rotation);
                assert_eq!(entry["unit"], symbol.unit);
                if symbol.reference() == Some("U999") {
                    assert_eq!(symbol.unit, 2);
                    assert_ne!(
                        symbol.at.x, 100.1,
                        "response must report grid-snapped coordinates"
                    );
                }
                if name == "add_power_symbol" {
                    assert_eq!(entry["added_power"], symbol.value_str().unwrap());
                }
            }
            for original in existing {
                let saved = committed
                    .symbols
                    .iter()
                    .find(|symbol| symbol.uuid == original.uuid)
                    .unwrap();
                assert_eq!(
                    saved.to_sexp(),
                    original.to_sexp(),
                    "existing symbol changed"
                );
            }
            assert_eq!(std::fs::read(&root).unwrap(), root_before);
            assert_eq!(
                std::fs::read(root.with_extension("kicad_pro")).unwrap(),
                project_before
            );
        }
    }
}

#[tokio::test]
async fn native_library_value_and_footprint_reach_the_placed_instance() {
    let (_directory, _root, child) = fixture(false);
    let result = add_native_component(
        &child,
        json!({
            "lib_id": "complex_hierarchy:MPSA42",
            "reference": "Q999",
            "x": 100.0,
            "y": 80.0
        }),
    )
    .await;
    assert!(!result.is_error, "{result:?}");
    let response = body(&result);
    assert_eq!(response["fields"]["Value"], "MPSA42");
    assert_eq!(response["fields"]["Footprint"], "TO92-CBE");

    let committed = Schematic::load(&child).unwrap();
    let symbol = committed.symbols.by_reference("Q999").unwrap();
    assert_eq!(symbol.value_str(), Some("MPSA42"));
    assert_eq!(symbol.footprint(), Some("TO92-CBE"));
}

/// The fixture is a KiCad-authored schematic whose embedded MPSA42 symbol has
/// a non-empty library Footprint. This live check proves KiCad's own netlister
/// consumes the placed instance field rather than merely trusting our readback.
#[tokio::test]
#[ignore = "needs an installed KiCad 10 kicad-cli"]
async fn kicad_netlist_contains_the_library_footprint_after_placement() {
    let (_directory, _root, child) = fixture(false);
    let result = add_native_component(
        &child,
        json!({
            "lib_id": "complex_hierarchy:MPSA42",
            "reference": "Q999",
            "x": 100.0,
            "y": 80.0
        }),
    )
    .await;
    assert!(!result.is_error, "{result:?}");

    let output = child.with_extension("net");
    let cli = crate::kicad_install::find_cli("").expect("installed kicad-cli");
    crate::tools::cli::export_netlist(&cli.display().to_string(), &child, &output, "kicadsexpr")
        .await
        .unwrap();
    let netlist = std::fs::read_to_string(output).unwrap();
    let q999 = netlist
        .split("(comp")
        .find(|component| component.contains("(ref \"Q999\")"))
        .expect("Q999 in netlist");
    assert!(
        q999.contains("(footprint \"TO92-CBE\")"),
        "KiCad must emit the library footprint for Q999:\n{q999}"
    );
}

#[tokio::test]
async fn native_placement_refuses_stale_instance_metadata_without_writing() {
    for corruption in [
        "missing",
        "foreign",
        "duplicate",
        "obsolete",
        "malformed",
        "missing-reference",
        "wrong-reference",
        "missing-unit",
        "wrong-unit",
    ] {
        for name in PLACERS {
            let (_directory, root, child) = fixture(true);
            let mut schematic = Schematic::load(&child).unwrap();
            let symbol = schematic.symbols.get_mut(0).unwrap();
            let instances = symbol
                .raw_sub_nodes
                .iter_mut()
                .find(|node| node.tag() == Some("instances"))
                .unwrap();
            let project = instances.find_mut("project").unwrap();
            let path = project.find("path").unwrap().clone();
            let SexpNode::List(children) = project else {
                unreachable!()
            };
            let index = children
                .iter()
                .position(|node| node.tag() == Some("path"))
                .unwrap();
            match corruption {
                "missing" => {
                    children.remove(index);
                }
                "foreign" => children[1] = SexpNode::Str("foreign-project".to_string()),
                "duplicate" => children.push(path),
                "obsolete" => {
                    let SexpNode::List(fields) = &mut children[index] else {
                        unreachable!()
                    };
                    fields[1] = SexpNode::Str("/obsolete/sheet".to_string());
                }
                "malformed" => {
                    let SexpNode::List(fields) = &mut children[index] else {
                        unreachable!()
                    };
                    fields.remove(1);
                }
                "missing-reference" => {
                    let SexpNode::List(fields) = &mut children[index] else {
                        unreachable!()
                    };
                    fields.retain(|field| field.tag() != Some("reference"));
                }
                "wrong-reference" => {
                    let SexpNode::List(fields) = children[index].find_mut("reference").unwrap()
                    else {
                        unreachable!()
                    };
                    fields[1] = SexpNode::Str("R999".to_string());
                }
                "missing-unit" => {
                    let SexpNode::List(fields) = &mut children[index] else {
                        unreachable!()
                    };
                    fields.retain(|field| field.tag() != Some("unit"));
                }
                "wrong-unit" => {
                    let SexpNode::List(fields) = children[index].find_mut("unit").unwrap() else {
                        unreachable!()
                    };
                    fields[1] = SexpNode::Atom("99".to_string());
                }
                _ => unreachable!(),
            }
            schematic.overwrite().unwrap();
            let before = std::fs::read(&child).unwrap();
            let root_before = std::fs::read(&root).unwrap();
            let result = place(name, &child).await;
            assert!(result.is_error, "{name}/{corruption}: {result:?}");
            assert_eq!(
                body(&result)["error"]["kind"],
                "stale_target",
                "{name}/{corruption}"
            );
            assert_eq!(
                std::fs::read(&child).unwrap(),
                before,
                "{name}/{corruption} wrote the target"
            );
            assert_eq!(std::fs::read(&root).unwrap(), root_before);
        }
    }
}

/// Rewrite every placed symbol's saved project name to `project_name(index)`,
/// reproducing #592: a whole sheet gone stale at once, the way copying a
/// `.kicad_sch` to a new filename stem leaves it. Returns the symbol count.
fn restamp_projects(child: &Path, project_name: impl Fn(usize) -> String) -> usize {
    let mut schematic = Schematic::load(child).unwrap();
    for (index, symbol) in schematic.symbols.iter_mut().enumerate() {
        let project = symbol
            .raw_sub_nodes
            .iter_mut()
            .find(|node| node.tag() == Some("instances"))
            .unwrap()
            .find_mut("project")
            .unwrap();
        let SexpNode::List(children) = project else {
            unreachable!()
        };
        children[1] = SexpNode::Str(project_name(index));
    }
    schematic.overwrite().unwrap();
    schematic.symbols.len()
}

/// Drive a placement through the served `tools/call` dispatch and return the
/// response text a client actually pays for, with its parsed body.
async fn served_place(child: &Path) -> (String, Value) {
    let handler = crate::mcp::handler::McpHandler::new(ServerConfig {
        kicad_cli: String::new(),
        kicad_binary: String::new(),
        ipc_address: String::new(),
        project_dir: None,
        jlcpcb_db_path: None,
        auto_load_toolsets: true,
        eager_toolsets: false,
    })
    .await
    .unwrap();
    let result = handler
        .handle_message(json!({"jsonrpc": "2.0", "id": 592, "method": "tools/call",
            "params": {"name": "add_power_symbol", "arguments": {
                "schematic": child.display().to_string(),
                "power_net": "GND", "x": 100.1, "y": 80.2}}}))
        .await
        .unwrap()
        .result
        .unwrap();
    let text = result["content"][0]["text"].as_str().unwrap().to_string();
    let body = serde_json::from_str(&text).unwrap();
    (text, body)
}

/// Place into an already-stale sheet, assert the refusal names the requested
/// schematic and wrote neither file, and return the response text with its
/// `error.reason`.
async fn served_place_refused(root: &Path, child: &Path) -> (String, String) {
    let before = std::fs::read(child).unwrap();
    let root_before = std::fs::read(root).unwrap();
    let (text, body) = served_place(child).await;
    assert_eq!(body["error"]["kind"], "stale_target");
    assert_eq!(
        body["error"]["target"],
        json!(child.display().to_string()),
        "the refusal must still name the requested schematic"
    );
    assert_eq!(std::fs::read(child).unwrap(), before, "wrote the target");
    assert_eq!(std::fs::read(root).unwrap(), root_before, "wrote the root");
    let reason = body["error"]["reason"].as_str().unwrap().to_string();
    (text, reason)
}

/// A sheet whose symbols all disagree the same way states the disagreement
/// once, with a bounded sample and a count, rather than once per symbol.
#[tokio::test]
async fn native_placement_collapses_a_uniform_stale_diagnosis() {
    let (_directory, root, child) = fixture(true);
    let symbols = restamp_projects(&child, |_| "foreign-project".to_string());
    assert!(symbols > PROMISED_IDENTITY_SAMPLE, "{symbols} symbols");

    let (text, reason) = served_place_refused(&root, &child).await;
    assert!(
        reason.contains(&format!("{symbols} of {symbols} placed symbols are stale")),
        "{reason}"
    );
    assert!(
        reason.contains(&format!(
            "and {} more ({symbols} symbols)",
            symbols - PROMISED_IDENTITY_SAMPLE
        )),
        "{reason}"
    );
    assert_eq!(
        reason.matches("observed [").count(),
        1,
        "one shared diagnosis, stated once: {reason}"
    );
    assert!(
        reason.contains("complex_hierarchy") && reason.contains("foreign-project"),
        "the refusal must still name both project identities: {reason}"
    );
    // The pre-fix reason grew by ~131 B per placed symbol. Bound the whole
    // response instead, so a larger sheet cannot enlarge the refusal.
    assert!(
        text.len() < 2_000,
        "{} B response for {symbols} symbols:\n{text}",
        text.len()
    );
}

/// A sheet whose symbols each fail differently still answers in bounded size:
/// the distinct diagnoses themselves are sampled and the rest counted.
#[tokio::test]
async fn native_placement_bounds_the_number_of_distinct_diagnoses() {
    let (_directory, root, child) = fixture(true);
    let symbols = restamp_projects(&child, |index| format!("foreign-{index}"));
    assert!(symbols > PROMISED_DIAGNOSIS_SAMPLE, "{symbols} symbols");

    let (text, reason) = served_place_refused(&root, &child).await;
    assert_eq!(
        reason.matches("observed [").count(),
        PROMISED_DIAGNOSIS_SAMPLE,
        "{reason}"
    );
    assert!(
        reason.contains(&format!(
            "and {} further distinct diagnoses",
            symbols - PROMISED_DIAGNOSIS_SAMPLE
        )),
        "{reason}"
    );
    assert!(text.len() < 3_500, "{} B response:\n{text}", text.len());
}

/// Sheet UUIDs chosen so the four lowest-sorting instance paths are common to
/// both stale symbols below; the two they disagree about sort last.
const EXTRA_SHEETS: [&str; 4] = [
    "aaaa0000-0000-0000-0000-000000000003",
    "aaaa0000-0000-0000-0000-000000000004",
    "ffff0000-0000-0000-0000-000000000005",
    "ffff0000-0000-0000-0000-000000000006",
];

/// Instantiate the child four more times in the root and give every placed
/// symbol a saved path for each new instance, cloning one of its own path
/// entries so reference and unit stay correct. The sheet then resolves to six
/// hierarchy instances with nothing stale.
fn widen_hierarchy(root: &Path, child: &Path) {
    let mut parent = Schematic::load(root).unwrap();
    let template = parent.sheets.get(0).unwrap().clone();
    for uuid in EXTRA_SHEETS {
        let mut sheet = template.clone();
        sheet.uuid = uuid.to_string();
        parent.sheets.push(sheet);
    }
    parent.overwrite().unwrap();

    let root_uuid = PATH_A.rsplit_once('/').unwrap().0;
    let mut schematic = Schematic::load(child).unwrap();
    for symbol in schematic.symbols.iter_mut() {
        let project = symbol
            .raw_sub_nodes
            .iter_mut()
            .find(|node| node.tag() == Some("instances"))
            .unwrap()
            .find_mut("project")
            .unwrap();
        let template = project.find("path").unwrap().clone();
        let SexpNode::List(children) = project else {
            unreachable!()
        };
        for uuid in EXTRA_SHEETS {
            let mut entry = template.clone();
            let SexpNode::List(fields) = &mut entry else {
                unreachable!()
            };
            fields[1] = SexpNode::Str(format!("{root_uuid}/{uuid}"));
            children.push(entry);
        }
    }
    schematic.overwrite().unwrap();
}

/// Drop the saved path ending in `sheet_uuid` from symbol `index`.
fn drop_instance_path(child: &Path, index: usize, sheet_uuid: &str) -> String {
    let mut schematic = Schematic::load(child).unwrap();
    let symbol = schematic.symbols.get_mut(index).unwrap();
    let identity = symbol.reference().unwrap().to_string();
    let project = symbol
        .raw_sub_nodes
        .iter_mut()
        .find(|node| node.tag() == Some("instances"))
        .unwrap()
        .find_mut("project")
        .unwrap();
    let SexpNode::List(children) = project else {
        unreachable!()
    };
    let before = children.len();
    children.retain(|node| {
        node.tag() != Some("path") || !node.value().unwrap_or_default().ends_with(sheet_uuid)
    });
    assert_eq!(children.len(), before - 1, "no path ended in {sheet_uuid}");
    schematic.overwrite().unwrap();
    identity
}

/// A sheet reused more than a handful of times must still be repairable from
/// the refusal: the expected instance list is what the caller has to write
/// back, so it is never sampled. Two symbols missing a different one of those
/// instances are stale in different ways, even when the difference falls past
/// the entries a sample would have shown.
#[tokio::test]
async fn native_placement_never_samples_the_expected_instance_list() {
    let (_directory, root, child) = fixture(true);
    widen_hierarchy(&root, &child);
    let clean = served_place(&child).await.1;
    assert!(
        clean["error"].is_null(),
        "widening must leave the sheet placeable: {clean}"
    );

    let (_directory, root, child) = fixture(true);
    widen_hierarchy(&root, &child);
    let first = drop_instance_path(&child, 0, EXTRA_SHEETS[2]);
    let second = drop_instance_path(&child, 1, EXTRA_SHEETS[3]);

    let (_, reason) = served_place_refused(&root, &child).await;
    let symbols = Schematic::load(&child).unwrap().symbols.len();
    assert!(
        reason.contains(&format!("2 of {symbols} placed symbols are stale")),
        "{reason}"
    );
    // Every instance the caller must restore is named, none elided behind a count.
    for uuid in EXTRA_SHEETS {
        assert!(
            reason.contains(uuid),
            "expected instance {uuid} elided: {reason}"
        );
    }
    assert!(!reason.contains("and 2 more"), "{reason}");
    // The two symbols disagree only past the fourth sorted path, so a sampled
    // grouping key would merge them into one diagnosis.
    assert_eq!(
        reason.matches("observed [").count(),
        2,
        "unlike failures merged: {reason}"
    );
    assert!(
        reason.contains(&first) && reason.contains(&second),
        "{reason}"
    );
}

/// Bounding must not merge unlike failures, and must not depend on run order.
#[tokio::test]
async fn native_placement_keeps_distinct_stale_diagnoses() {
    let (_directory, root, child) = fixture(true);
    let mut schematic = Schematic::load(&child).unwrap();
    let mut stale = Vec::new();
    for (index, symbol) in schematic.symbols.iter_mut().enumerate().take(2) {
        stale.push(symbol.reference().unwrap().to_string());
        let project = symbol
            .raw_sub_nodes
            .iter_mut()
            .find(|node| node.tag() == Some("instances"))
            .unwrap()
            .find_mut("project")
            .unwrap();
        if index == 0 {
            let SexpNode::List(children) = project else {
                unreachable!()
            };
            children[1] = SexpNode::Str("foreign-project".to_string());
        } else {
            let instance = project.find_mut("path").unwrap();
            let SexpNode::List(fields) = instance.find_mut("unit").unwrap() else {
                unreachable!()
            };
            fields[1] = SexpNode::Atom("99".to_string());
        }
    }
    schematic.overwrite().unwrap();
    let symbols = schematic.symbols.len();

    let (_, reason) = served_place_refused(&root, &child).await;
    assert!(
        reason.contains(&format!("2 of {symbols} placed symbols are stale")),
        "{reason}"
    );
    for identity in &stale {
        assert!(reason.contains(identity.as_str()), "{identity}: {reason}");
    }
    assert!(reason.contains("observed ["), "{reason}");
    assert!(
        reason.contains("instance unit disagrees with symbol unit"),
        "unlike failures must survive the collapse: {reason}"
    );

    // Same input, same bytes: the grouping is keyed by diagnosis but ordered by
    // the document, not by hash iteration.
    let (_, repeated) = served_place_refused(&root, &child).await;
    assert_eq!(repeated, reason);
}

#[tokio::test]
async fn native_placement_refuses_ambiguous_ownership_without_writing() {
    for name in PLACERS {
        let (directory, root, child) = fixture(true);
        let competing = directory.path().join("competing.kicad_sch");
        std::fs::copy(&root, &competing).unwrap();
        std::fs::copy(
            root.with_extension("kicad_pro"),
            competing.with_extension("kicad_pro"),
        )
        .unwrap();
        let before = std::fs::read(&child).unwrap();
        let root_before = std::fs::read(&root).unwrap();
        let result = place(name, &child).await;
        assert!(result.is_error, "{name}: {result:?}");
        assert_eq!(body(&result)["error"]["kind"], "conflict");
        assert_eq!(std::fs::read(&child).unwrap(), before);
        assert_eq!(std::fs::read(&root).unwrap(), root_before);
        assert_eq!(std::fs::read(&competing).unwrap(), root_before);
    }
}

#[test]
fn native_placement_readback_refuses_wrong_document_and_missing_evidence() {
    for missing in [
        "document",
        "symbol",
        "Reference",
        "Value",
        "Footprint",
        "instances",
        "stale",
    ] {
        let (_directory, _root, child) = fixture(true);
        let mut schematic = Schematic::load(&child).unwrap();
        let context = sheet_instance_context(&child, &mut schematic).unwrap();
        let uuid = schematic.symbols.get(0).unwrap().uuid.clone();
        let symbol = schematic.symbols.get(0).unwrap();
        let fields = sch_components::PlacementFields {
            value: symbol.value_str().unwrap_or_default().to_string(),
            footprint: symbol.footprint().unwrap_or_default().to_string(),
        };
        let expected = sch_components::ComponentTargetUnit::placement(
            &uuid,
            &context,
            &symbol.lib_id,
            symbol.at.x,
            symbol.at.y,
            symbol.at.rotation.unwrap_or(0.0),
            symbol.mirror.as_deref(),
            symbol.reference().unwrap(),
            &fields,
            symbol.unit,
        );
        match missing {
            "symbol" => {
                schematic.symbols.remove_by_uuid(&uuid).unwrap();
            }
            "Reference" | "Value" | "Footprint" => schematic
                .symbols
                .get_mut(0)
                .unwrap()
                .remove_property(missing),
            "instances" => schematic
                .symbols
                .get_mut(0)
                .unwrap()
                .raw_sub_nodes
                .retain(|node| node.tag() != Some("instances")),
            "stale" => schematic
                .symbols
                .get_mut(0)
                .unwrap()
                .set_instance_path("foreign", "/foreign", "U999", 1),
            "document" => {}
            _ => unreachable!(),
        }
        schematic.overwrite().unwrap();
        let other = child.with_file_name("other.kicad_sch");
        std::fs::copy(&child, &other).unwrap();
        let committed = Schematic::load(if missing == "document" {
            &other
        } else {
            &child
        })
        .unwrap();
        let result =
            sch_components::placed_component_readback(&child, &committed, &expected, &context)
                .expect_err("incomplete readback must not return success");
        assert_eq!(body(&result)["error"]["kind"], "stale_target", "{missing}");
        assert!(!body(&result)["message"]
            .as_str()
            .unwrap()
            .contains("did not modify"));
    }
}

#[test]
fn native_placement_readback_requires_requested_values() {
    for mismatch in ["unit", "lib_id", "x", "y", "rotation", "Value", "Footprint"] {
        let (_directory, _root, child) = fixture(true);
        let mut schematic = Schematic::load(&child).unwrap();
        let context = sheet_instance_context(&child, &mut schematic).unwrap();
        let symbol = schematic.symbols.get(0).unwrap();
        let fields = sch_components::PlacementFields {
            value: if mismatch == "Value" {
                "wrong-value".to_string()
            } else {
                symbol.value_str().unwrap_or_default().to_string()
            },
            footprint: if mismatch == "Footprint" {
                "wrong:footprint".to_string()
            } else {
                symbol.footprint().unwrap_or_default().to_string()
            },
        };
        let expected = sch_components::ComponentTargetUnit::placement(
            &symbol.uuid,
            &context,
            if mismatch == "lib_id" {
                "Device:WRONG"
            } else {
                &symbol.lib_id
            },
            symbol.at.x + if mismatch == "x" { 1.27 } else { 0.0 },
            symbol.at.y + if mismatch == "y" { 1.27 } else { 0.0 },
            symbol.at.rotation.unwrap_or(0.0) + if mismatch == "rotation" { 90.0 } else { 0.0 },
            symbol.mirror.as_deref(),
            symbol.reference().unwrap(),
            &fields,
            symbol.unit + u32::from(mismatch == "unit"),
        );
        let committed = Schematic::load(&child).unwrap();
        let error =
            sch_components::placed_component_readback(&child, &committed, &expected, &context)
                .unwrap_err();
        assert_eq!(body(&error)["error"]["kind"], "stale_target", "{mismatch}");
    }
}
