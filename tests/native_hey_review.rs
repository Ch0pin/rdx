//! Optional diagnostics for the reviewer-supplied APK; no APK is committed.
use rdx::engine::{DecompilerEngine, MethodSourceFormat, NativeEngine};
#[test]
#[ignore = "requires RDX_HEY_APK and RDX_HEY_OUTPUT"]
fn hey_review_diagnostics() {
    let apk = std::path::PathBuf::from(std::env::var_os("RDX_HEY_APK").unwrap());
    let out = std::path::PathBuf::from(std::env::var_os("RDX_HEY_OUTPUT").unwrap());
    std::fs::create_dir_all(&out).unwrap();
    let mut e = NativeEngine::start().unwrap();
    let project = e.open(&apk).unwrap();
    let mut reports = vec![];
    for name in &project.classes {
        let interesting = name.contains("DownloadsWorker")
            || name == "f3.e"
            || name.ends_with(".web.g")
            || name == "web.g";
        let c = e.dex_class(name).unwrap();
        let registration = c.methods.iter().any(|m| {
            m.code.as_ref().is_some_and(|code| {
                rdx::native_ir::DecodedMethod::decode(code).is_ok_and(|ir| {
                    ir.instructions.iter().any(|i| {
                        matches!(i.opcode, 0x6e..=0x72 | 0x74..=0x78)
                            && i.reference.is_some_and(|r| {
                                c.symbols
                                    .methods
                                    .get(r.index as usize)
                                    .is_some_and(|(_, _, n)| {
                                        c.symbols.strings[*n as usize]
                                            == "registerFragmentDestinations"
                                    })
                            })
                    })
                })
            })
        });
        let hotwire = registration
            || name.contains("NavigatorGraphBuilder")
            || name.contains("HotwireDestination")
            || name == "com.basecamp.hey.d"
            || name == "c4.c"
            || name == "ui.c"
            || name == "m9.g"
            || name == "ui.a"
            || name == "ui.b"
            || name == "l9.b"
            || name == "kotlin.text.o"
            || name == "kotlin.jvm.internal.i";
        if interesting || hotwire {
            let c = e.dex_class(name).unwrap();
            let ids: Vec<String> = c
                .methods
                .iter()
                .filter(|m| m.code.is_some())
                .map(|m| {
                    format!(
                        "{name}.{}({}){}",
                        m.name,
                        m.parameters.join(""),
                        m.return_type
                    )
                })
                .collect();
            let mut dex = String::new();
            for id in &ids {
                dex.push_str(
                    &e.method_source(id, MethodSourceFormat::Dex, &false.into())
                        .unwrap()
                        .code
                        .source,
                );
                dex.push('\n');
            }
            std::fs::write(out.join(format!("{name}.dex.txt")), dex).unwrap();
            if interesting {
                for id in ids {
                    let r = e
                        .method_source(&id, MethodSourceFormat::Auto, &false.into())
                        .unwrap();
                    std::fs::write(
                        out.join(format!("{}.java.txt", id.replace('/', "_"))),
                        &r.code.source,
                    )
                    .unwrap();
                    reports.push(
                        serde_json::json!({"id":id,"format":r.format,"reason":r.fallback_reason}),
                    );
                }
            }
        }
    }
    std::fs::write(
        out.join("reports.json"),
        serde_json::to_string_pretty(&reports).unwrap(),
    )
    .unwrap();
    for suffix in [
        "DownloadsWorker.e(Ljava/lang/String;)Lci/m;",
        "f3.e.invoke(Ljava/lang/Object;)Ljava/lang/Object;",
        "web.g.handleMessage(Landroid/os/Message;)V",
    ] {
        let row = reports
            .iter()
            .find(|r| r["id"].as_str().unwrap().ends_with(suffix))
            .unwrap();
        assert_eq!(row["format"], "java", "{row}");
    }
    let config = rdx::config_consumers::search(
        &e,
        &project,
        "external_navigable_hosts",
        0,
        20,
        &false.into(),
    )
    .unwrap();
    std::fs::write(
        out.join("config-before.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
}
