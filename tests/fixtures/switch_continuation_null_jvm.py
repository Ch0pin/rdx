#!/usr/bin/env python3
"""Shared zero fanout oracle; raw registers distinguish null from boxed zero."""

from switch_continuation_exit_jvm import main, oracle as execute_words


TYPES = ["String", "Integer", "Long", "Double", "Float", "Short", "Byte", "Boolean", "Character"]


def reference_hook(raw, opcode, method, argument):
    assert opcode == 0x71 and method["owner"] == raw["class"]
    name = method["name"]
    result_type = method.get("return", method.get("return_type"))
    if name in [f"null{i}" for i in range(len(TYPES))]:
        index = int(name[4:])
        assert method["parameters"] == [f"Ljava/lang/{TYPES[index]};"]
        assert result_type == "V"
        assert type(argument) is int and argument == 0, (name, argument)
        return name, None
    if name == "observe":
        assert method["parameters"] == ["Ljava/lang/Integer;"] and result_type == "I"
        if type(argument) is int and argument == 0:
            return "null", -1
        assert isinstance(argument, tuple) and argument[0] == "Integer"
        return f"Integer:{argument[1]}", argument[1]
    raise AssertionError(f"unsupported reference hook {method}")


def oracle(raw, count, mode, throw_at):
    return execute_words(raw, count, mode, throw_at, reference_hook=reference_hook)


HOOKS = """
    static void hit(String value) {
        if (events.length() != 0) events.append(',');
        events.append(value);
        if (ordinal++ == throwAt) throw MARKER;
    }
    public static int observe(Integer value) {
        hit(value == null ? "null" : "Integer:" + value.intValue());
        return value == null ? -1 : value.intValue();
    }
""" + "\n".join(
    f'    public static void null{index}({ty} value) {{ if(value != null) throw new AssertionError("nonnull fanout"); hit("null{index}"); }}'
    for index, ty in enumerate(TYPES)
)


if __name__ == "__main__":
    main(
        oracle_fn=oracle,
        hook_source=HOOKS,
        scope="unchanged emitted method; literal DEX null fanout to nine reference signatures plus numeric zero; null versus boxed Integer carry, deferred String exit, exact effects and marker identity",
    )
