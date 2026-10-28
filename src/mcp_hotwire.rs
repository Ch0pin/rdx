//! Hotwire annotation inventory and bounded ordered class-array evidence.
//! Array positions are evidence, not a claim that a runtime graph registered them.
use super::*;
use crate::{
    native_dex::{DexClass, DexMethod, DexValue},
    native_engine::disassembly::{method_id, symbol, type_name},
    native_ir::DecodedMethod,
};
use std::collections::{BTreeMap, BTreeSet};
const ANNOTATION: &str = "Ldev/hotwire/navigation/destinations/HotwireDestinationDeepLink;";
#[derive(Clone, Debug, PartialEq, Eq)]
enum Atom {
    Unknown,
    Class(String),
    Wrapped(String),
    Int(i64),
    Array(usize),
}
fn ordered_arrays(
    c: &DexClass,
    m: &DexMethod,
    known: &BTreeSet<String>,
    cancel: &AtomicBool,
) -> Result<Vec<Value>> {
    let Some(code) = &m.code else {
        return Ok(vec![]);
    };
    ensure!(
        code.instructions.len() <= 32768 && code.registers <= 512,
        "Hotwire array method budget exceeded"
    );
    let ir = DecodedMethod::decode(code)?;
    let mut regs = vec![Atom::Unknown; code.registers as usize];
    let mut arrays: BTreeMap<usize, Vec<Option<String>>> = BTreeMap::new();
    let mut result = Atom::Unknown;
    let mut rows = vec![];
    for i in &ir.instructions {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        // Only a straight-line prefix is certified. Branches/handlers need CFG proofs.
        if matches!(i.opcode,0x0e..=0x11|0x1d..=0x1e|0x27..=0x2c|0x32..=0x3d)
            || code
                .try_regions
                .iter()
                .any(|t| i.pc >= t.start as usize && i.pc < t.end as usize)
        {
            break;
        }
        let get = |n: usize| {
            i.reads
                .get(n)
                .map(|r| regs[r.register as usize].clone())
                .unwrap_or(Atom::Unknown)
        };
        let mut value = match i.opcode {
            0x01..=0x09 | 0x1f => get(0),
            0x0a..=0x0c => result.clone(),
            0x12..=0x15 => i.literal.map(Atom::Int).unwrap_or(Atom::Unknown),
            0x1c => i
                .reference
                .and_then(|r| c.symbols.types.get(r.index as usize))
                .map(|s| Atom::Class(type_name(s)))
                .unwrap_or(Atom::Unknown),
            0x23 => {
                if let Atom::Int(n) = get(0) {
                    if (1..=512).contains(&n) {
                        arrays.insert(i.pc, vec![None; n as usize]);
                        Atom::Array(i.pc)
                    } else {
                        Atom::Unknown
                    }
                } else {
                    Atom::Unknown
                }
            }
            _ => Atom::Unknown,
        };
        result = Atom::Unknown;
        if i.opcode == 0x4d {
            if let (Atom::Wrapped(name), Atom::Array(a), Atom::Int(index)) =
                (get(2), get(0), get(1))
            {
                if let Some(v) = arrays
                    .get_mut(&a)
                    .and_then(|v| usize::try_from(index).ok().and_then(|n| v.get_mut(n)))
                {
                    *v = Some(name);
                } else {
                    arrays.remove(&a);
                }
            } else if let Atom::Array(a) = get(0) {
                arrays.remove(&a);
            }
        }
        if matches!(i.opcode, 0x24 | 0x25) {
            let values: Vec<_> = (0..i.reads.len())
                .map(|n| match get(n) {
                    Atom::Wrapped(s) => Some(s),
                    _ => None,
                })
                .collect();
            if !values.is_empty() && values.len() <= 512 {
                arrays.insert(i.pc, values);
                result = Atom::Array(i.pc);
            }
        }
        if matches!(i.opcode,0x6e..=0x72|0x74..=0x78) {
            let target = i
                .reference
                .and_then(|r| symbol(&c.symbols, 'm', r.index as usize))
                .unwrap_or_default();
            // Kotlin class wrappers are explicitly labelled as a library model.
            let start = usize::from(!matches!(i.opcode, 0x71 | 0x77));
            if target.starts_with("kotlin.jvm.internal.")
                && target.contains("(Ljava/lang/Class;)L")
                && let Atom::Class(s) = get(start)
            {
                value = Atom::Wrapped(s);
            }
            for n in 0..i.reads.len() {
                if let Atom::Array(a) = get(n) {
                    if let Some(values) = arrays.get(&a)
                        && values.iter().all(Option::is_some)
                        && values.iter().flatten().any(|s| known.contains(s))
                    {
                        rows.push(json!({"method_id":method_id(m),"call_site_offset":i.pc,"target_method_id":target,"argument_word":n,"array_offset":a,"classes":values,"evidence":"straight-line class-array construction; Kotlin class wrapper model","registration_call_candidate":target.contains(".registerFragmentDestinations(")}));
                    }
                    // An arbitrary callee can mutate the array; later calls are unproven.
                    arrays.remove(&a);
                }
            }
            result = value.clone();
        }
        for w in &i.writes {
            regs[w.register as usize] = value.clone();
        }
    }
    Ok(rows)
}
// Certify the library's simple index-to-route loop, without assuming every
// candidate class array reaches this builder at runtime.
fn numbering(c: &DexClass, m: &DexMethod) -> Option<Value> {
    let code = m.code.as_ref()?;
    if !code.try_regions.is_empty() || code.instructions.len() > 512 {
        return None;
    }
    let ir = DecodedMethod::decode(code).ok()?;
    let ins = &ir.instructions;
    let calls = |i: &crate::native_ir::Instruction| {
        i.reference
            .and_then(|r| symbol(&c.symbols, 'm', r.index as usize))
    };
    let back: Vec<_> = ins
        .iter()
        .filter(|i| matches!(i.opcode, 0x28..=0x2a) && i.branch_target.is_some_and(|t| t < i.pc))
        .collect();
    let [back] = back.as_slice() else { return None };
    let start = back.branch_target?;
    let body: Vec<_> = ins
        .iter()
        .filter(|i| i.pc >= start && i.pc <= back.pc)
        .collect();
    if body
        .iter()
        .any(|i| matches!(i.opcode, 0x28..=0x2a) && i.pc != back.pc)
    {
        return None;
    }
    if body
        .iter()
        .filter(|i| calls(i).as_deref() == Some("java.util.Iterator.next()Ljava/lang/Object;"))
        .count()
        != 1
    {
        return None;
    }
    let branches: Vec<_> = body
        .iter()
        .filter(|i| matches!(i.opcode, 0x2b | 0x2c | 0x32..=0x3d))
        .collect();
    let [exit] = branches.as_slice() else {
        return None;
    };
    if exit.opcode != 0x38 || exit.branch_target? <= back.pc {
        return None;
    }
    let increment: Vec<_> = body
        .iter()
        .filter(|i| i.opcode == 0xd8 && i.literal == Some(1))
        .collect();
    let [inc] = increment.as_slice() else {
        return None;
    };
    let counter = inc.reads.first()?.register;
    let next = inc.writes.first()?.register;
    let init = ins
        .iter()
        .rev()
        .find(|i| i.pc < start && i.writes.iter().any(|w| w.register == counter))?;
    if !matches!(init.opcode, 0x12..=0x15) || init.literal != Some(1) {
        return None;
    }
    let convert = body
        .iter()
        .find(|i| calls(i).as_deref() == Some("java.lang.String.valueOf(I)Ljava/lang/String;"))?;
    if convert.reads.first()?.register != counter || convert.pc <= inc.pc {
        return None;
    }
    let route = ins.iter().find(|i| i.pc == convert.pc + convert.width)?;
    if route.opcode != 0x0c {
        return None;
    }
    let route_reg = route.writes.first()?.register;
    let constructor=body.iter().find(|i|calls(i).is_some_and(|s|s.starts_with("dev.hotwire.navigation.navigator.NavigatorGraphBuilder$FragmentDestination.<init>(Ljava/lang/String;Landroid/net/Uri;")))?;
    if constructor.reads.get(1)?.register != route_reg || constructor.pc <= route.pc {
        return None;
    }
    if body.iter().any(|i| {
        i.pc > route.pc && i.pc < constructor.pc && i.writes.iter().any(|w| w.register == route_reg)
    }) {
        return None;
    }
    let update = ins.iter().find(|i| i.pc + i.width == back.pc)?;
    if !matches!(update.opcode, 0x01..=0x03)
        || update.reads.first()?.register != next
        || update.writes.first()?.register != counter
    {
        return None;
    }
    if body
        .iter()
        .any(|i| i.pc != inc.pc && i.pc != update.pc && i.writes.iter().any(|w| w.register == next))
    {
        return None;
    }
    if body
        .iter()
        .any(|i| i.pc < convert.pc && i.writes.iter().any(|w| w.register == counter))
    {
        return None;
    }
    if !body
        .iter()
        .any(|i| calls(i).as_deref() == Some("java.util.Iterator.next()Ljava/lang/Object;"))
    {
        return None;
    }
    Some(
        json!({"status":"verified_counter_loop","method_id":method_id(m),"first_route":1,"step":1,"conversion_offset":convert.pc,"constructor_offset":constructor.pc,"scope":"Ordinal route string for each iteration of this builder; candidate array registration remains separate"}),
    )
}

pub(super) fn append(
    engine: &NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
    page: &mut scan::Page,
) -> Result<Value> {
    let mut destinations = BTreeMap::new();
    for name in &project.classes {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let Some(c) = engine.dex_class(name) else {
            continue;
        };
        if let Some(set) = c
            .symbols
            .annotations
            .get(&c.annotations_offset)
            .and_then(|d| d.class.as_ref())
        {
            for a in set.iter() {
                if c.symbols
                    .types
                    .get(a.type_idx as usize)
                    .is_some_and(|s| s.as_ref() == ANNOTATION)
                {
                    for (key, value) in &a.elements {
                        if c.symbols
                            .strings
                            .get(*key as usize)
                            .is_some_and(|s| s == "uri")
                            && let DexValue::String(index) = value
                            && let Some(uri) = c.symbols.strings.get(*index as usize)
                        {
                            destinations.insert(name.clone(), uri.clone());
                        }
                    }
                }
            }
        }
    }
    let route_numbering = engine
        .dex_class("dev.hotwire.navigation.navigator.NavigatorGraphBuilder")
        .and_then(|c| {
            c.methods
                .iter()
                .find(|m| {
                    m.name.as_ref() == "build"
                        && m.parameters.iter().map(|p| p.as_ref()).collect::<Vec<_>>()
                            == ["Ljava/util/List;"]
                })
                .and_then(|m| numbering(c, m))
        });
    let known: BTreeSet<_> = destinations.keys().cloned().collect();
    let mut evidence: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut errors = vec![];
    let mut failed = 0;
    let mut truncated_arrays = 0usize;
    let mut pools = std::collections::HashMap::new();
    for name in &project.classes {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let Some(c) = engine.dex_class(name) else {
            continue;
        };
        // Pool check is only a cheap filter; instruction operands prove actual uses.
        if !*pools
            .entry(Arc::as_ptr(&c.symbols) as usize)
            .or_insert_with(|| {
                c.symbols
                    .types
                    .iter()
                    .any(|t| known.contains(&type_name(t)))
            })
        {
            continue;
        }
        for m in &c.methods {
            let Some(code) = &m.code else { continue };
            if !code.instructions.iter().any(|w| *w as u8 == 0x1c) {
                continue;
            }
            match ordered_arrays(c, m, &known, cancel) {
                Ok(rows) => {
                    for row in rows {
                        for (index, class) in row["classes"].as_array().unwrap().iter().enumerate()
                        {
                            if let Some(class) = class.as_str().filter(|s| known.contains(*s)) {
                                let entries = evidence.entry(class.to_owned()).or_default();
                                if entries.len() < 64 {
                                    let mut r = row.clone();
                                    r.as_object_mut().unwrap().remove("classes");
                                    r["array_index"] = json!(index);
                                    r["route_id"] = Value::Null;
                                    if route_numbering.is_some() {
                                        r["route_if_registered_in_this_order"] =
                                            json!((index + 1).to_string());
                                    }
                                    r["route_status"] = json!(
                                        "unresolved: registration-to-builder flow and route assignment require proof"
                                    );
                                    entries.push(r);
                                } else {
                                    truncated_arrays += 1;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                    failed += 1;
                    if errors.len() < 30 {
                        errors.push(json!({"method_id":method_id(m),"reason":e.to_string()}));
                    }
                }
            }
        }
    }
    let count = destinations.len();
    for (name, uri) in destinations {
        if !scan::package_matches(&name, args) {
            continue;
        }
        page.add(json!({"kind":"hotwire_annotation","route_numbering":route_numbering,"destination_class":name,"uri_pattern":uri,"annotation_type":type_name(ANNOTATION),"registration_arrays":evidence.remove(&name).unwrap_or_default(),"runtime_registration_proven":false,"arguments":[],"offset_unit":"dex_code_units"}))?;
    }
    Ok(
        json!({"annotated_destinations":count,"array_analysis_errors":errors,"failed_array_methods":failed,"truncated_array_evidence":truncated_arrays,"coverage":"Exact class annotations; bounded straight-line array construction. Array indexes are not navigation IDs. Dynamic registration and unsupported array/control flow remain unresolved."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexCode, DexSymbols};
    fn fixture() -> DexClass {
        let mut c = crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        c.symbols = Arc::new(DexSymbols {
            types: vec![
                "Lsample/Screen;".into(),
                "Lkotlin/jvm/internal/Reflection;".into(),
                "Lsample/Registry;".into(),
                "[Lkotlin/reflect/KClass;".into(),
            ],
            strings: vec!["wrap".into(), "registerFragmentDestinations".into()],
            protos: vec![
                (
                    "Lkotlin/reflect/KClass;".into(),
                    vec!["Ljava/lang/Class;".into()],
                ),
                ("V".into(), vec!["[Lkotlin/reflect/KClass;".into()]),
            ],
            methods: vec![(1, 0, 0), (2, 1, 1)],
            ..Default::default()
        });
        c.methods[0].code = Some(DexCode {
            registers: 4,
            ins: 0,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![
                0x001c, 0, 0x1071, 0, 0, 0x000c, // KClass
                0x1112, 0x1223, 3, 0x0312, 0x004d, 0x0302, // array[0] = KClass
                0x1071, 1, 2, 0x1071, 1, 2, 0x000e, // second call sees possibly mutated array
            ],
        });
        c
    }
    #[test]
    fn ordered_array_tracks_slot_and_invalidates_after_callee_escape() {
        let c = fixture();
        let known = BTreeSet::from(["sample.Screen".to_owned()]);
        let rows = ordered_arrays(&c, &c.methods[0], &known, &AtomicBool::new(false)).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["classes"], json!(["sample.Screen"]));
        assert_eq!(rows[0]["call_site_offset"], 12);
        assert_eq!(rows[0]["registration_call_candidate"], true);
        let mut c = fixture();
        c.methods[0].code.as_mut().unwrap().instructions[9] = 0x1312; // invalid slot
        assert!(
            ordered_arrays(&c, &c.methods[0], &known, &AtomicBool::new(false))
                .unwrap()
                .is_empty()
        );
        let mut c = fixture();
        c.methods[0].code.as_mut().unwrap().instructions[9] = 0x0328; // branch before write
        assert!(
            ordered_arrays(&c, &c.methods[0], &known, &AtomicBool::new(false))
                .unwrap()
                .is_empty()
        );
        assert!(ordered_arrays(&c, &c.methods[0], &known, &AtomicBool::new(true)).is_err());
    }
    #[test]
    #[ignore = "requires RDX_HEY_APK and RDX_HEY_OUTPUT"]
    fn hey_hotwire_inventory_contains_annotation_and_order_evidence() {
        let mut engine = NativeEngine::start().unwrap();
        let project = engine
            .open(Path::new(&std::env::var("RDX_HEY_APK").unwrap()))
            .unwrap();
        let result = super::super::xml_discovery::search(
            &mut engine,
            &project,
            "list_nav_deeplinks",
            &json!({"limit":500}),
            &AtomicBool::new(false),
        )
        .unwrap();
        fs::write(
            Path::new(&std::env::var("RDX_HEY_OUTPUT").unwrap()).join("hotwire.json"),
            serde_json::to_string_pretty(&result).unwrap(),
        )
        .unwrap();
        let row = result["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["uri_pattern"] == "hotwire://fragment/web/composer")
            .expect("composer annotation");
        assert_eq!(row["kind"], "hotwire_annotation");
        assert_eq!(
            row["route_numbering"]["status"], "verified_counter_loop",
            "{row}"
        );
        assert!(
            row["registration_arrays"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["array_index"] == 3 && a["route_if_registered_in_this_order"] == "4"),
            "{row}"
        );
    }
}
