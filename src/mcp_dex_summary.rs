//! Optional bounded Bundle read evidence. No reconstructed Java or taint inference.
use super::*;
use crate::native_ir::DecodedMethod;

pub(super) fn summarize(engine: &NativeEngine, id: &str, cancel: &AtomicBool) -> Result<Value> {
    let class = engine
        .dex_class(crate::method_lookup::owner(id)?)
        .context("Method owner unavailable")?;
    let method = crate::method_lookup::resolve(class, id)?;
    summarize_method(class, method, id, cancel)
}
fn summarize_method(
    class: &crate::native_dex::DexClass,
    method: &crate::native_dex::DexMethod,
    id: &str,
    cancel: &AtomicBool,
) -> Result<Value> {
    let inputs = crate::native_engine::disassembly::input_registers(method);
    let Some(code) = method.code.as_ref() else {
        return Ok(json!({"input_registers":inputs,"bundle_reads":[],"status":"no_code"}));
    };
    if code.instructions.len() > 32768 || code.registers > 512 {
        return Ok(
            json!({"input_registers":inputs,"bundle_reads":[],"status":"method_budget_exceeded","complete":false}),
        );
    }
    let ir = DecodedMethod::decode(code)?;
    let sites = crate::native_xrefs::method_call_sites(class, method, cancel)?;
    let mut matching = Vec::new();
    for site in &sites {
        if ["android.os.Bundle.", "android.os.BaseBundle."]
            .iter()
            .any(|prefix| site.callee.starts_with(prefix))
            && (site.callee.contains(".getString(Ljava/lang/String;")
                || site.callee.contains(".getBoolean(Ljava/lang/String;"))
        {
            matching.push(site);
        }
    }
    let total = matching.len();
    matching.truncate(32);
    let wanted = matching.iter().map(|s| s.pc).collect();
    let mut work = 0;
    let constants = if matching.is_empty() {
        Ok(std::collections::BTreeMap::new())
    } else {
        crate::call_arguments::analyze(class, method, cancel, &mut work, &wanted)
    };
    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
    let mut rows = Vec::new();
    for site in matching {
        let index = ir
            .instructions
            .binary_search_by_key(&site.pc, |i| i.pc)
            .ok()
            .context("Call offset unavailable")?;
        let instruction = &ir.instructions[index];
        let result = ir
            .instructions
            .get(index + 1)
            .filter(|i| i.pc == site.pc + instruction.width && matches!(i.opcode, 0x0a..=0x0c))
            .and_then(|i| i.writes.first())
            .map(|r| r.register);
        let proof = constants.as_ref().ok().and_then(|m| m.get(&site.pc));
        let key = proof
            .and_then(|p| p["arguments"].as_array())
            .and_then(|a| a.first());
        let value = key
            .filter(|k| k["status"] == "constant" && k["kind"] == "string")
            .map(|k| k["value"].clone());
        rows.push(json!({"method_id":id,"call_site_offset":site.pc,"callee_method_id":site.callee,"key":value,"key_status":if value.is_some(){"constant"}else{"unresolved"},"key_register":key.and_then(|k|k["register"].as_u64()),"result_register":result,"result_used":result.is_some()}));
    }
    Ok(
        json!({"input_registers":inputs,"bundle_reads":rows,"matching_reads":total,"complete":total<=32 && constants.is_ok(),"status":if constants.is_ok(){"analyzed"}else{"constant_analysis_unavailable"},"reason":constants.err().map(|e|format!("{e:#}")),"scope":"Exact Bundle/BaseBundle getString/getBoolean calls; at most 32 reads. Keys require constant agreement across reachable paths. Result registers describe the immediate move-result only, not subsequent flow or trust."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bundle_key_is_proven_and_result_register_is_exact() {
        let mut c = crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        c.methods.retain(|m| m.name.as_ref() == "answer");
        c.symbols = Arc::new(crate::native_dex::DexSymbols {
            strings: vec!["enabled".into(), "getBoolean".into()],
            types: vec!["Landroid/os/Bundle;".into()],
            protos: vec![("Z".into(), vec!["Ljava/lang/String;".into(), "Z".into()])],
            methods: vec![(0, 0, 1)],
            ..Default::default()
        });
        let m = &mut c.methods[0];
        m.access_flags = 9;
        m.parameters = vec!["Landroid/os/Bundle;".into()];
        m.return_type = "V".into();
        let code = m.code.as_mut().unwrap();
        code.registers = 3;
        code.ins = 1;
        code.outs = 3;
        code.instructions = vec![0x001a, 0, 0x0112, 0x306e, 0, 0x0102, 0x000a, 0x000e];
        let cancel = AtomicBool::new(false);
        let result = summarize_method(
            &c,
            &c.methods[0],
            "sample.Hello.answer(Landroid/os/Bundle;)V",
            &cancel,
        )
        .unwrap();
        assert_eq!(result["bundle_reads"][0]["key"], "enabled");
        assert_eq!(result["bundle_reads"][0]["call_site_offset"], 3);
        assert_eq!(result["bundle_reads"][0]["result_register"], 0);
        // Overwriting the key register destroys the string proof.
        c.methods[0].code.as_mut().unwrap().instructions =
            vec![0x001a, 0, 0x0012, 0x306e, 0, 0x0102, 0x000a, 0x000e];
        let result = summarize_method(
            &c,
            &c.methods[0],
            "sample.Hello.answer(Landroid/os/Bundle;)V",
            &cancel,
        )
        .unwrap();
        assert_eq!(result["bundle_reads"][0]["key_status"], "unresolved");
    }
    #[test]
    fn parameter_aliases_account_for_receiver_static_and_wide_words() {
        let mut c = crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        let m = &mut c.methods[0];
        m.access_flags = 1;
        m.parameters = vec!["J".into(), "Ljava/lang/String;".into(), "D".into()];
        let code = m.code.as_mut().unwrap();
        code.registers = 8;
        code.ins = 6;
        let a = crate::native_engine::disassembly::input_registers(m);
        assert_eq!(a["aliases"][0]["register"], 2);
        assert_eq!(a["aliases"][0]["receiver"], true);
        assert_eq!(a["aliases"][2]["parameter_index"], 0);
        assert_eq!(a["aliases"][2]["word"], 1);
        assert_eq!(a["aliases"][3]["parameter_index"], 1);
        m.access_flags = 9;
        m.code.as_mut().unwrap().ins = 5;
        let a = crate::native_engine::disassembly::input_registers(m);
        assert_eq!(a["aliases"][0]["receiver"], false);
        assert_eq!(a["aliases"][0]["register"], 3);
        m.code.as_mut().unwrap().ins = 9;
        assert_eq!(
            crate::native_engine::disassembly::input_registers(m)["status"],
            "invalid_input_count"
        );
    }
}
