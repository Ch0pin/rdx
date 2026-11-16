pub const ACCESS_ORDER: [u32; 17] = [
    0x1, 0x4, 0x2, 0x400, 0x8, 0x10, 0x80, 0x40, 0x20, 0x100, 0x800, 0x200, 0x1000,
    0x2000, 0x4000, 0x10000, 0x20000,
];

pub fn descriptor_to_java(descriptor: &str) -> String {
    descriptor_to_java_mode(descriptor, false)
}

pub fn descriptor_to_java_enhanced(descriptor: &str) -> String {
    descriptor_to_java_mode(descriptor, true)
}

fn descriptor_to_java_mode(descriptor: &str, enhanced: bool) -> String {
    if let Some(element) = descriptor.strip_prefix('[') {
        return format!("{}[]", descriptor_to_java_mode(element, enhanced));
    }
    match descriptor {
        "V" => "void".to_owned(),
        "Z" => "boolean".to_owned(),
        "B" => "byte".to_owned(),
        "S" => "short".to_owned(),
        "C" => "char".to_owned(),
        "I" => "int".to_owned(),
        "J" => "long".to_owned(),
        "F" => "float".to_owned(),
        "D" => "double".to_owned(),
        "O" => "Object".to_owned(),
        _ if descriptor.starts_with('L') && descriptor.ends_with(';') => {
            let class = descriptor[1..descriptor.len() - 1].replace('/', ".");
            if enhanced {
                if let Some(rest) = class.strip_prefix("java.lang.")
                    && !rest.contains('.')
                {
                    rest.to_owned()
                } else {
                    class
                }
            } else {
                class
                    .strip_prefix("java.lang.")
                    .unwrap_or(&class)
                    .to_owned()
            }
        }
        _ => descriptor.to_owned(),
    }
}

pub fn split_method_descriptor(descriptor: &str) -> Option<(Vec<&str>, &str)> {
    let close = descriptor.find(')')?;
    let parameters = descriptor
        .get(1..close)?
        .split_whitespace()
        .filter(|value| !value.is_empty())
        .collect();
    let return_type = descriptor.get(close + 1..).filter(|value| !value.is_empty())?;
    Some((parameters, return_type))
}

pub fn class_access(flags: u32) -> Vec<String> {
    access(flags, AccessTarget::Class)
}

pub fn field_access(flags: u32) -> Vec<String> {
    access(flags, AccessTarget::Field)
}

pub fn method_access(flags: u32) -> Vec<String> {
    access(flags, AccessTarget::Method)
}

enum AccessTarget {
    Class,
    Field,
    Method,
}

fn access(flags: u32, target: AccessTarget) -> Vec<String> {
    ACCESS_ORDER
        .iter()
        .copied()
        .filter(|flag| flags & flag != 0)
        .map(|flag| {
            let name = match target {
                AccessTarget::Class => match flag {
                0x1 => "public",
                0x2 => "private",
                0x4 => "protected",
                0x8 => "static",
                0x10 => "final",
                0x200 => "interface",
                0x400 => "abstract",
                0x1000 => "synthetic",
                0x2000 => "annotation",
                0x4000 => "enum",
                    _ => return format!("unkn_{flag}"),
                },
                AccessTarget::Field => match flag {
                0x1 => "public",
                0x2 => "private",
                0x4 => "protected",
                0x8 => "static",
                0x10 => "final",
                0x40 => "volatile",
                0x80 => "transient",
                0x1000 => "synthetic",
                0x4000 => "enum",
                    _ => return format!("unkn_{flag}"),
                },
                AccessTarget::Method => match flag {
                0x1 => "public",
                0x2 => "private",
                0x4 => "protected",
                0x8 => "static",
                0x10 => "final",
                0x20 => "synchronized",
                0x40 => "bridge",
                0x80 => "varargs",
                0x100 => "native",
                0x400 => "abstract",
                0x800 => "strictfp",
                0x1000 => "synthetic",
                0x10000 => "constructor",
                0x20000 => "declared_synchronized",
                    _ => return format!("unkn_{flag}"),
                },
            };
            name.to_owned()
        })
        .collect()
}
