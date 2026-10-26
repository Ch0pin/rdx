//! Focused may-flow evidence for WebView chooser callbacks. Never an exploit verdict.
use super::*;
use crate::{
    native_cfg::{ControlFlowGraph, EdgeKind},
    native_dex::{DexClass, DexMethod},
    native_engine::disassembly,
    native_ir::{DecodedMethod, Instruction},
};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
const CALLBACK: u8 = 1;
const RESULT: u8 = 2;
const URI: u8 = 4;
const CLIP: u8 = 8;
const UNKNOWN_URI: u8 = 16;
const CONTRACT_RESULT: u8 = 32;
#[derive(Default)]
struct Evidence {
    stores: BTreeMap<String, Vec<Value>>,
    sinks: Vec<Value>,
    sources: Vec<Value>,
    checks: Vec<Value>,
}
fn field(c: &DexClass, i: &Instruction) -> Option<String> {
    let (owner, ty, name) = *c.symbols.fields.get(i.reference?.index as usize)?;
    Some(format!(
        "{}.{}:{}",
        disassembly::type_name(c.symbols.types.get(owner as usize)?),
        c.symbols.strings.get(name as usize)?,
        c.symbols.types.get(ty as usize)?
    ))
}
#[derive(Clone)]
struct Accessor {
    field: String,
    argument: Option<usize>,
    offset: usize,
}
fn accessor(c: &DexClass, m: &DexMethod) -> Option<Accessor> {
    let code = m.code.as_ref()?;
    if !code.try_regions.is_empty()
        || code.instructions.len() > 24
        || !(m.return_type.as_ref() == "Landroid/webkit/ValueCallback;"
            || m.parameters
                .iter()
                .any(|p| p.as_ref() == "Landroid/webkit/ValueCallback;"))
    {
        return None;
    }
    let ir = DecodedMethod::decode(code).ok()?;
    let mut origins = vec![None; code.registers as usize];
    for (word, r) in ((code.registers - code.ins)..code.registers).enumerate() {
        origins[r as usize] = Some(word);
    }
    let mut found = None;
    let mut getter_register = None;
    let mut returned = false;
    for i in &ir.instructions {
        if returned {
            return None;
        }
        match i.opcode {
            0 => {}
            0x07..=0x09 => {
                let src = i.reads.first()?.register;
                let dst = i.writes.first()?.register;
                origins[dst as usize] = origins[src as usize];
                if getter_register == Some(src) {
                    getter_register = Some(dst);
                } else if getter_register == Some(dst) {
                    getter_register = None;
                }
            }
            0x54 | 0x62 => {
                if found.is_some() {
                    return None;
                }
                let f = field(c, i)?;
                if !f.ends_with(":Landroid/webkit/ValueCallback;") {
                    return None;
                }
                if i.opcode == 0x54 {
                    origins[i.reads.first()?.register as usize]?;
                }
                found = Some(Accessor {
                    field: f,
                    argument: None,
                    offset: i.pc,
                });
                getter_register = Some(i.writes.first()?.register);
                origins[i.writes.first()?.register as usize] = None;
            }
            0x5b | 0x69 => {
                if found.is_some() {
                    return None;
                }
                let f = field(c, i)?;
                if !f.ends_with(":Landroid/webkit/ValueCallback;") {
                    return None;
                }
                if i.opcode == 0x5b {
                    origins[i.reads.first()?.register as usize]?;
                }
                let argument =
                    origins[i.reads.get(usize::from(i.opcode == 0x5b))?.register as usize]?;
                found = Some(Accessor {
                    field: f,
                    argument: Some(argument),
                    offset: i.pc,
                });
            }
            0x0e => {
                if found.as_ref()?.argument.is_none() || m.return_type.as_ref() != "V" {
                    return None;
                }
                returned = true;
            }
            0x11 => {
                if found.as_ref()?.argument.is_some()
                    || getter_register != Some(i.reads.first()?.register)
                {
                    return None;
                }
                returned = true;
            }
            _ => return None,
        }
    }
    if returned { found } else { None }
}
fn chooser(m: &DexMethod) -> bool {
    m.name.as_ref() == "onShowFileChooser"
        && m.return_type.as_ref() == "Z"
        && m.parameters.iter().map(|p| p.as_ref()).eq([
            "Landroid/webkit/WebView;",
            "Landroid/webkit/ValueCallback;",
            "Landroid/webkit/WebChromeClient$FileChooserParams;",
        ])
}
fn initial(c: &DexClass, m: &DexMethod) -> Vec<u8> {
    let code = m.code.as_ref().unwrap();
    let mut state = vec![0; code.registers as usize + 1];
    let mut reg = (code.registers - code.ins) as usize + usize::from(m.access_flags & 8 == 0);
    for p in &m.parameters {
        if chooser(m) && p.as_ref() == "Landroid/webkit/ValueCallback;" {
            state[reg] = CALLBACK;
        }
        if m.name.as_ref() == "onActivityResult"
            && ((m.parameters.iter().map(|p| p.as_ref()).eq([
                "I",
                "I",
                "Landroid/content/Intent;",
            ]) && p.as_ref() == "Landroid/content/Intent;")
                || p.as_ref() == "Landroidx/activity/result/ActivityResult;")
        {
            state[reg] = RESULT;
        }
        if m.name.as_ref() == "onActivityResult"
            && m.parameters.len() == 1
            && c.interfaces
                .iter()
                .any(|i| i.as_ref() == "Landroidx/activity/result/ActivityResultCallback;")
        {
            state[reg] = match p.as_ref() {
                "Landroid/net/Uri;" | "[Landroid/net/Uri;" => URI,
                "Landroidx/activity/result/ActivityResult;" => RESULT,
                _ => CONTRACT_RESULT,
            };
        }
        reg += if matches!(p.as_ref(), "J" | "D") {
            2
        } else {
            1
        };
    }
    state
}
fn transfer(
    c: &DexClass,
    m: &DexMethod,
    i: &Instruction,
    state: &mut [u8],
    fields: &BTreeMap<String, Vec<Value>>,
    accessors: &BTreeMap<String, Accessor>,
    e: &mut Evidence,
) -> Result<()> {
    let pending = state.len() - 1;
    let read = |n: usize| {
        i.reads
            .get(n)
            .map(|r| state[r.register as usize])
            .unwrap_or(0)
    };
    let mut value = 0;
    match i.opcode {
        0x07..=0x09 => value = read(0),
        0x1f => {
            value = read(0);
            if value & CONTRACT_RESULT != 0 {
                value = match i
                    .reference
                    .and_then(|r| c.symbols.types.get(r.index as usize))
                    .map(|s| s.as_ref())
                {
                    Some("Landroid/net/Uri;" | "[Landroid/net/Uri;") => URI,
                    Some("Landroidx/activity/result/ActivityResult;") => RESULT,
                    _ => value,
                };
            }
        }
        0x0c => value = state[pending],
        0x54 | 0x62 => {
            if field(c, i).is_some_and(|f| fields.contains_key(&f)) {
                value = CALLBACK;
            }
        }
        0x5b | 0x69 => {
            if read(usize::from(i.opcode == 0x5b)) & CALLBACK != 0
                && let Some(f) = field(c, i)
            {
                e.stores
                    .entry(f)
                    .or_default()
                    .push(json!({"method_id":disassembly::method_id(m),"offset":i.pc}));
            }
        }
        0x4d => {
            // IR reads array, index, value for aput-object.
            if let Some(array) = i.reads.first() {
                state[array.register as usize] |= read(2) & (URI | UNKNOWN_URI);
            }
        }
        0x46 => value = read(0) & (URI | UNKNOWN_URI),
        0x24 | 0x25 => {
            value = i
                .reads
                .iter()
                .fold(0, |a, r| a | state[r.register as usize]);
            state[pending] = value;
        }
        0x6e..=0x72 | 0x74..=0x78 => {
            if let Some(site) = crate::native_xrefs::call_site_at(c, m, i.pc)? {
                let t = site.callee.as_str();
                let receiver = read(0);
                let mut returned = 0;
                if let Some(a) = accessors.get(t) {
                    if let Some(arg) = a.argument {
                        if read(arg) & CALLBACK != 0 {
                            e.stores.entry(a.field.clone()).or_default().push(json!({"method_id":disassembly::method_id(m),"offset":i.pc,"via_verified_accessor":t,"accessor_field_offset":a.offset}));
                        }
                    } else if fields.contains_key(&a.field) {
                        returned = CALLBACK;
                    }
                }
                if t == "android.content.Intent.getData()Landroid/net/Uri;" {
                    returned = if receiver & RESULT != 0 {
                        URI
                    } else {
                        UNKNOWN_URI
                    };
                }
                if t == "android.content.Intent.getClipData()Landroid/content/ClipData;"
                    && receiver & RESULT != 0
                {
                    returned = CLIP;
                }
                if t == "android.content.ClipData.getItemAt(I)Landroid/content/ClipData$Item;"
                    && receiver & CLIP != 0
                {
                    returned = CLIP;
                }
                if t == "android.content.ClipData$Item.getUri()Landroid/net/Uri;"
                    && receiver & CLIP != 0
                {
                    returned = URI;
                }
                if t == "androidx.activity.result.ActivityResult.getData()Landroid/content/Intent;"
                    && receiver & RESULT != 0
                {
                    returned = RESULT;
                }
                if t == "android.webkit.WebChromeClient$FileChooserParams.parseResult(ILandroid/content/Intent;)[Landroid/net/Uri;"
                {
                    returned = if read(1) & RESULT != 0 {
                        URI
                    } else {
                        UNKNOWN_URI
                    };
                }
                if returned & URI != 0 {
                    e.sources.push(
                        json!({"offset":i.pc,"target":t,"origin":"external_activity_result"}),
                    );
                }
                if t == "android.webkit.ValueCallback.onReceiveValue(Ljava/lang/Object;)V" {
                    e.sinks.push(json!({"offset":i.pc,"target":t,"external_uri_may_flow":read(1)&URI!=0,"uri_origin_unresolved":read(1)&UNKNOWN_URI!=0,"chooser_callback_field_or_parameter":receiver&CALLBACK!=0}));
                }
                if t.starts_with("android.net.Uri.getScheme(")
                    || t.starts_with("android.net.Uri.getAuthority(")
                    || t.starts_with("android.content.ContentResolver.getType(")
                    || t.contains(".getCanonicalPath(")
                {
                    e.checks.push(json!({"offset":i.pc,"target":t,"effect":"validation-related call; adequacy and dominance unproven"}));
                }
                state[pending] = returned;
            }
        }
        _ => {}
    }
    if !matches!(i.opcode,0x24|0x25|0x6e..=0x72|0x74..=0x78) {
        state[pending] = 0;
    }
    for w in &i.writes {
        for n in 0..w.kind.word_count() {
            state[w.register as usize + n] = value;
        }
    }
    Ok(())
}
fn analyze(
    c: &DexClass,
    m: &DexMethod,
    fields: &BTreeMap<String, Vec<Value>>,
    accessors: &BTreeMap<String, Accessor>,
    cancel: &AtomicBool,
) -> Result<Evidence> {
    let code = m.code.as_ref().context("No code")?;
    ensure!(
        code.instructions.len() <= 32768 && code.registers <= 512,
        "Chooser method analysis budget exceeded"
    );
    let ir = DecodedMethod::decode(code)?;
    let cfg = ControlFlowGraph::build(code)?;
    ensure!(
        cfg.blocks.len() * (code.registers as usize + 1) <= 200000,
        "Chooser state budget exceeded"
    );
    let instructions: HashMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let mut states = vec![None; cfg.blocks.len()];
    let entry = *cfg.block_at.get(&0).context("No entry")?;
    states[entry] = Some(initial(c, m));
    let mut queue = VecDeque::from([entry]);
    let mut queued = vec![false; states.len()];
    queued[entry] = true;
    let mut work = 0;
    while let Some(b) = queue.pop_front() {
        queued[b] = false;
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let mut state = states[b].clone().unwrap();
        for pc in &cfg.blocks[b].instructions {
            work += 1;
            ensure!(work <= 2000000, "Chooser fixed-point work limit exceeded");
            transfer(
                c,
                m,
                instructions[pc],
                &mut state,
                fields,
                accessors,
                &mut Evidence::default(),
            )?;
        }
        for edge in &cfg.blocks[b].successors {
            if edge.kind == EdgeKind::Exceptional {
                continue;
            }
            let changed = if let Some(old) = &mut states[edge.target] {
                let mut changed = false;
                for (a, b) in old.iter_mut().zip(&state) {
                    let v = *a | *b;
                    if v != *a {
                        *a = v;
                        changed = true;
                    }
                }
                changed
            } else {
                states[edge.target] = Some(state.clone());
                true
            };
            if changed && !queued[edge.target] {
                queue.push_back(edge.target);
                queued[edge.target] = true;
            }
        }
    }
    let mut evidence = Evidence::default();
    for (b, input) in cfg.blocks.iter().zip(states) {
        if let Some(mut state) = input {
            for pc in &b.instructions {
                transfer(
                    c,
                    m,
                    instructions[pc],
                    &mut state,
                    fields,
                    accessors,
                    &mut evidence,
                )?;
            }
        }
    }
    Ok(evidence)
}
pub(super) fn search(
    engine: &mut NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    let mut accessors = BTreeMap::new();
    for name in &project.classes {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let c = engine.dex_class(name).context("Missing class")?;
        for m in &c.methods {
            if let Some(a) = accessor(c, m) {
                accessors.insert(disassembly::method_id(m), a);
            }
        }
    }
    let mut fields = BTreeMap::new();
    let mut callbacks = Vec::new();
    let mut errors = Vec::new();
    let mut error_count = 0;
    // Discover callback storage across the project before applying result-package filtering.
    for name in &project.classes {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let c = engine.dex_class(name).context("Missing class")?;
        for m in c.methods.iter().filter(|m| chooser(m) && m.code.is_some()) {
            match analyze(c, m, &BTreeMap::new(), &accessors, cancel) {
                Ok(e) => {
                    for (f, sites) in e.stores {
                        fields.entry(f).or_insert_with(Vec::new).extend(sites);
                    }
                    callbacks.push(disassembly::method_id(m));
                }
                Err(e) => {
                    error_count += 1;
                    if errors.len() < 50 {
                        errors.push(
                            json!({"method_id":disassembly::method_id(m),"error":format!("{e:#}")}),
                        );
                    }
                }
            }
        }
    }
    let mut page = scan::Page::new(args)?;
    for name in &project.classes {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        if !scan::package_matches(name, args) {
            continue;
        }
        let c = engine.dex_class(name).context("Missing class")?;
        for m in &c.methods {
            if m.code.is_none() {
                continue;
            }
            let sites = crate::native_xrefs::method_call_sites(c, m, cancel)?;
            if !chooser(m)
                && !sites.iter().any(|s| {
                    s.callee == "android.webkit.ValueCallback.onReceiveValue(Ljava/lang/Object;)V"
                })
            {
                continue;
            }
            let id = disassembly::method_id(m);
            match analyze(c, m, &fields, &accessors, cancel) {
                Ok(e) => {
                    let known = chooser(m)
                        || e.sinks
                            .iter()
                            .any(|s| s["chooser_callback_field_or_parameter"] == true);
                    if !known
                        && !e.sinks.iter().any(|s| {
                            s["external_uri_may_flow"] == true || s["uri_origin_unresolved"] == true
                        })
                    {
                        continue;
                    }
                    let correlated = e.sinks.iter().any(|s| {
                        s["external_uri_may_flow"] == true
                            && s["chooser_callback_field_or_parameter"] == true
                    });
                    let code = m.code.as_ref().unwrap();
                    let ir = DecodedMethod::decode(code)?;
                    let literals:Vec<_>=ir.instructions.iter().filter(|i|matches!(i.opcode,0x1a|0x1b)).filter_map(|i| {
                        let value=c.symbols.strings.get(i.reference?.index as usize)?;
                        (value.starts_with("file:") || value.starts_with("content:") || ["file","content","android.intent.action.GET_CONTENT","android.intent.action.OPEN_DOCUMENT"].contains(&value.as_str())).then(||json!({"offset":i.pc,"value":value.chars().take(4096).collect::<String>(),"truncated":value.chars().count()>4096,"relationship":"literal_evidence_only"}))
                    }).collect();
                    let launch_sites:Vec<_>=sites.iter().filter(|s|s.callee.contains(".startActivityForResult(") || s.callee.starts_with("androidx.activity.result.ActivityResultLauncher.launch(") || s.callee.starts_with("android.webkit.WebChromeClient$FileChooserParams.createIntent(")).map(|s|json!({"offset":s.pc,"target":s.callee})).collect();
                    let mut linked: BTreeSet<_> = ir
                        .instructions
                        .iter()
                        .filter_map(|i| {
                            if matches!(i.opcode, 0x54 | 0x62 | 0x5b | 0x69) {
                                field(c, i).filter(|f| fields.contains_key(f))
                            } else {
                                None
                            }
                        })
                        .collect();
                    for site in &sites {
                        if let Some(a) = accessors.get(&site.callee)
                            && fields.contains_key(&a.field)
                        {
                            linked.insert(a.field.clone());
                        }
                    }
                    let stores: Vec<_> = linked
                        .iter()
                        .map(|f| json!({"field_id":f,"stores_from_chooser":fields[f]}))
                        .collect();
                    page.add(json!({"method_id":id,"dex_entry":c.symbols.origin.get(),"status":if correlated {"candidate_external_uri_to_chooser_callback"}else{"unresolved_chooser_flow"},"chooser_entry":chooser(m),"sources":e.sources,"sinks":e.sinks,"callback_field_associations":stores,"validation_evidence":e.checks,"uri_and_action_literals":literals,"chooser_launch_sites":launch_sites,"private_file_path_resolution":"Result-supplied URI path is runtime data; declared literals are evidence only","requires_runtime_validation":true,"exception_paths_analyzed":false}))?;
                }
                Err(e) => {
                    error_count += 1;
                    if errors.len() < 50 {
                        errors.push(json!({"method_id":id,"error":format!("{e:#}")}));
                    }
                }
            }
        }
    }
    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
    let mut result = page.finish()?;
    result["complete"] = json!(error_count == 0);
    result["errors"] = json!(errors);
    result["failed_methods"] = json!(error_count);
    result["errors_truncated"] = json!(error_count > 50);
    result["chooser_entry_count"] = json!(callbacks.len());
    result["analysis_scope"] = json!(
        "Intraprocedural normal-edge may-flow, local arrays/moves/casts and direct callback fields with verified straight-line getter/setter accessors. Field associations span methods but object identity, path feasibility, validation adequacy, helper calls, aliases through heap containers and exceptional flows remain unresolved. Completion describes this scan, not whole-program security coverage."
    );
    result["capability_note"] = json!(
        "External chooser results may designate file URIs or target-owned content authorities. No FileProvider producer is required. Reachability, permissions, actual private-file reads and delivery to a web origin require runtime proof."
    );
    let manifest = project
        .resources
        .iter()
        .find(|p| p.as_str() == "AndroidManifest.xml")
        .or_else(|| {
            project
                .resources
                .iter()
                .find(|p| p.ends_with("/AndroidManifest.xml"))
        });
    if let Some(path) = manifest {
        match engine.read_resource(path).and_then(|xml| {
            crate::manifest_summary::query_resolved(
                &xml,
                &json!({"component_type":"provider"}),
                engine.resource_table(),
                true,
            )
            .map_err(anyhow::Error::msg)
        }) {
            Ok(mut providers) => {
                let count = providers.len();
                providers.truncate(100);
                crate::manifest_summary::project_records(
                    &mut providers,
                    &json!({"projection":"compact"}),
                );
                result["declared_provider_candidates"] = json!(providers);
                result["provider_count"] = json!(count);
                result["providers_truncated"] = json!(count > 100);
            }
            Err(e) => result["provider_inventory_error"] = json!(format!("{e:#}")),
        }
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexCode, DexSymbols};
    fn fixture() -> DexClass {
        let s = DexSymbols {
            types: vec![
                "Landroid/content/Intent;".into(),
                "[Landroid/net/Uri;".into(),
                "Landroid/webkit/ValueCallback;".into(),
                "Lsample/Chooser;".into(),
            ],
            strings: vec!["getData".into(), "onReceiveValue".into(), "callback".into()],
            protos: vec![
                ("Landroid/net/Uri;".into(), vec![]),
                ("V".into(), vec!["Ljava/lang/Object;".into()]),
            ],
            methods: vec![(0, 0, 0), (2, 1, 1)],
            fields: vec![(3, 2, 2)],
            ..Default::default()
        };
        let code = |registers, ins, instructions| {
            Some(DexCode {
                registers,
                ins,
                outs: 2,
                tries: 0,
                try_regions: vec![],
                instructions,
                offset: 0,
            })
        };
        let method = |name: &str, params: Vec<&str>, ret: &str, body| DexMethod {
            declaring_type: "Lsample/Chooser;".into(),
            name: name.into(),
            parameters: params.into_iter().map(Into::into).collect(),
            return_type: ret.into(),
            access_flags: 1,
            thrown_types: vec![],
            code: body,
        };
        let show = method(
            "onShowFileChooser",
            vec![
                "Landroid/webkit/WebView;",
                "Landroid/webkit/ValueCallback;",
                "Landroid/webkit/WebChromeClient$FileChooserParams;",
            ],
            "Z",
            code(5, 4, vec![0x135b, 0, 0x1012, 0x000f]),
        );
        let result = method(
            "onActivityResult",
            vec!["I", "I", "Landroid/content/Intent;"],
            "V",
            code(
                8,
                4,
                vec![
                    0x106e, 0, 7, 0x000c, 0x1112, 0x1123, 1, 0x0212, 0x004d, 0x0201, 0x4354, 0,
                    0x2072, 1, 0x0013, 0x000e,
                ],
            ),
        );
        DexClass {
            symbols: Arc::new(s),
            descriptor: "Lsample/Chooser;".into(),
            superclass: None,
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![show, result],
        }
    }
    #[test]
    fn tracks_external_uri_array_into_stored_chooser_callback() {
        let c = fixture();
        let cancel = AtomicBool::new(false);
        let show = analyze(
            &c,
            &c.methods[0],
            &BTreeMap::new(),
            &BTreeMap::new(),
            &cancel,
        )
        .unwrap();
        assert_eq!(show.stores.len(), 1);
        let result = analyze(&c, &c.methods[1], &show.stores, &BTreeMap::new(), &cancel).unwrap();
        assert_eq!(result.sinks[0]["external_uri_may_flow"], true);
        assert_eq!(result.sinks[0]["chooser_callback_field_or_parameter"], true);
        assert_eq!(result.sources[0]["offset"], 0);
        let unknown = analyze(
            &c,
            &c.methods[1],
            &BTreeMap::new(),
            &BTreeMap::new(),
            &cancel,
        )
        .unwrap();
        assert_eq!(
            unknown.sinks[0]["chooser_callback_field_or_parameter"],
            false
        );
    }
    #[test]
    fn verifies_simple_callback_setters_and_rejects_extra_work() {
        let mut c = fixture();
        let setter = DexMethod {
            declaring_type: c.descriptor.clone(),
            name: "setCallback".into(),
            return_type: "V".into(),
            parameters: vec![
                c.descriptor.clone(),
                "Landroid/webkit/ValueCallback;".into(),
            ],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 2,
                ins: 2,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                instructions: vec![0x015b, 0, 0x000e],
                offset: 0,
            }),
        };
        let a = accessor(&c, &setter).unwrap();
        assert_eq!(a.argument, Some(1));
        let symbols = Arc::get_mut(&mut c.symbols).unwrap();
        symbols.strings.push("setCallback".into());
        symbols.protos.push(("V".into(), setter.parameters.clone()));
        symbols.methods.push((3, 2, 3));
        c.methods[0].code.as_mut().unwrap().instructions = vec![0x2071, 2, 0x0031, 0x1012, 0x000f];
        let mut accessors = BTreeMap::new();
        accessors.insert(disassembly::method_id(&setter), a);
        let e = analyze(
            &c,
            &c.methods[0],
            &BTreeMap::new(),
            &accessors,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(e.stores.len(), 1);
        assert!(e.stores.values().next().unwrap()[0]["via_verified_accessor"].is_string());
        let mut setter = setter;
        setter.code.as_mut().unwrap().instructions.insert(0, 0x0012);
        assert!(accessor(&c, &setter).is_none());
    }
    #[test]
    fn arbitrary_intent_and_overwritten_uri_are_not_external_proof() {
        let mut c = fixture();
        c.methods[1].name = "ordinaryMethod".into();
        let e = analyze(
            &c,
            &c.methods[1],
            &BTreeMap::new(),
            &BTreeMap::new(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(e.sinks[0]["external_uri_may_flow"], false);
        assert_eq!(e.sinks[0]["uri_origin_unresolved"], true);
        c.methods[1].name = "onActivityResult".into();
        c.methods[1]
            .code
            .as_mut()
            .unwrap()
            .instructions
            .insert(8, 0x0012);
        let e = analyze(
            &c,
            &c.methods[1],
            &BTreeMap::new(),
            &BTreeMap::new(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(e.sinks[0]["external_uri_may_flow"], false);
    }
}
