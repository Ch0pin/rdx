use rdx::{native_dex, native_java};

fn fixture(ty: &str, width: u16, bytes: &[u8]) -> native_dex::DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    let method = &mut class.methods[0];
    method.name = "fill".into();
    method.parameters = vec![ty.into()];
    method.return_type = "V".into();
    method.access_flags = 9;
    let count = bytes.len() / usize::from(width);
    let code = method.code.as_mut().unwrap();
    code.registers = 1;
    code.ins = 1;
    code.outs = 0;
    code.tries = 0;
    code.try_regions.clear();
    code.instructions = vec![
        0x0026,
        4,
        0,
        0x000e,
        0x0300,
        width,
        count as u16,
        (count >> 16) as u16,
    ];
    code.instructions.extend(
        bytes
            .chunks(2)
            .map(|pair| u16::from(pair[0]) | (u16::from(*pair.get(1).unwrap_or(&0)) << 8)),
    );
    class
}
fn render(class: &native_dex::DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.Hello", class, &class.methods[0])?.source)
}

#[test]
fn primitive_payloads_preserve_bits_and_check_capacity_before_stores() {
    for (ty, width, bytes, expected) in [
        ("[Z", 1, vec![0, 1], "[1] = true;"),
        ("[B", 1, vec![0x80, 0xff, 0x7f], "[0] = -128;"),
        ("[S", 2, vec![0, 0x80], "[0] = -32768;"),
        ("[C", 2, vec![0xff, 0xff], "[0] = 65535;"),
        (
            "[I",
            4,
            i32::MIN.to_le_bytes().to_vec(),
            "[0] = -2147483648;",
        ),
        (
            "[J",
            8,
            i64::MIN.to_le_bytes().to_vec(),
            "[0] = -9223372036854775808L;",
        ),
        (
            "[F",
            4,
            0x80000000u32.to_le_bytes().to_vec(),
            "[0] = -0.0f;",
        ),
        (
            "[D",
            8,
            0x8000000000000000u64.to_le_bytes().to_vec(),
            "[0] = -0.0d;",
        ),
    ] {
        let source = render(&fixture(ty, width, &bytes)).unwrap();
        assert!(source.contains(expected), "{source}");
        assert!(
            source.find(".length").unwrap() < source.find("[0] =").unwrap(),
            "{source}"
        );
    }
    let source = render(&fixture("[F", 4, &0x7fc00001u32.to_le_bytes())).unwrap();
    assert!(source.contains("intBitsToFloat(0x7fc00001)"), "{source}");
    assert!(render(&fixture("[F", 4, &0x7f800001u32.to_le_bytes())).is_err());
}

#[test]
fn invalid_payloads_and_reference_arrays_are_rejected() {
    assert!(render(&fixture("[I", 2, &[1, 0])).is_err());
    assert!(render(&fixture("[Ljava/lang/Object;", 4, &[0, 0, 0, 0])).is_err());
    assert!(render(&fixture("[Z", 1, &[2])).is_err());
    let mut class = fixture("[I", 4, &[1, 0, 0, 0]);
    class.methods[0].code.as_mut().unwrap().instructions.pop();
    assert!(render(&class).is_err());
    let mut class = fixture("[I", 4, &[1, 0, 0, 0]);
    class.methods[0].code.as_mut().unwrap().instructions[1] = 5;
    assert!(render(&class).is_err());
}

#[test]
#[ignore = "requires javac and java"]
fn generated_java_preserves_bulk_failure_state_null_and_float_bits() {
    use std::{fs, process::Command};
    let ints = render(&fixture("[I", 4, &[1, 0, 0, 0, 2, 0, 0, 0])).unwrap();
    let floats = render(&fixture("[F", 4, &[0, 0, 0, 0x80, 1, 0, 0xc0, 0x7f])).unwrap();
    let empty = render(&fixture("[B", 1, &[])).unwrap();
    let source = format!(
        r#"public class ArrayPayloadChecks {{
{ints}
{floats}
{empty}
public static void main(String[] args) {{
 int[] a = {{9,9,7}}; fill(a);
 if (a[0]!=1 || a[1]!=2 || a[2]!=7) throw new AssertionError();
 int[] shortArray = {{9}};
 try {{ fill(shortArray); throw new AssertionError(); }} catch(ArrayIndexOutOfBoundsException expected) {{}}
 if (shortArray[0]!=9) throw new AssertionError("partial fill");
 try {{ fill((int[])null); throw new AssertionError(); }} catch(NullPointerException expected) {{}}
 try {{ fill((byte[])null); throw new AssertionError(); }} catch(NullPointerException expected) {{}}
 float[] f = new float[2]; fill(f);
 if(Float.floatToRawIntBits(f[0])!=0x80000000 || Float.floatToRawIntBits(f[1])!=0x7fc00001) throw new AssertionError();
 System.out.println("six array payload checks passed");
}}
}}"#
    );
    let directory = std::env::temp_dir().join(format!("rdx-array-payload-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("ArrayPayloadChecks.java"), source).unwrap();
    for (program, args) in [
        ("javac", vec!["ArrayPayloadChecks.java"]),
        ("java", vec!["-cp", ".", "ArrayPayloadChecks"]),
    ] {
        let output = Command::new(program)
            .args(args)
            .current_dir(&directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(directory).unwrap();
}
