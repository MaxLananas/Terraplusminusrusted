#!/usr/bin/env python3
"""Writes patches/delombok-osm.patch: the members Lombok would generate for the OpenStreetMap
sources the oracle compiles (constructors, fluent getters, builders, equals/hashCode).

Lombok cannot run in the authoring environment (no Maven, the redistributed jars are too old for
the JDK and javac's delombok needs jdk.compiler), so this reproduces the subset of Lombok the
sources use, following the project's lombok.config (fluent, chained accessors). Annotations stay
in place (source-retention stubs live in stubs/src/lombok); only generated members are added, plus
the two edits Lombok performs on the declarations themselves (@Builder.Default initialisers move
into the builder, @SneakyThrows becomes a throws clause on a method nothing in the oracle calls).
Not generated because nothing compiled for the oracle uses them: @ToString and @With.

Usage: delombok-osm.py <upstream package root> <output patch>
"""
import difflib
import pathlib
import re
import sys

SOURCES = [
    "dataset/geojson/*.java",
    "dataset/geojson/geometry",
    "dataset/geojson/object",
    "dataset/osm",
    "dataset/vector/draw",
    "dataset/vector/geometry",
    "util/bvh/BVH.java",
    "util/bvh/EmptyBVH.java",
    "util/bvh/QuadtreeBVH.java",
    "util/bvh/SingletonBVH.java",
    "util/interval",
    "substitutes/BlockState.java",
    "substitutes/BlockStateBuilder.java",
    "substitutes/BlockPropertyValue.java",
    "substitutes/Identifier.java",
    "substitutes/exceptions",
]

ANNOTATION = re.compile(r"@[\w.]+(?:\s*\((?:[^()]|\([^()]*\))*\))?")
PRIMITIVES = {"int", "long", "short", "byte", "char", "boolean", "double", "float"}


def skip_literal(text, i):
    """Index just past the string/char literal or comment starting at i, or i if none starts there."""
    if text.startswith("//", i):
        end = text.find("\n", i)
        return len(text) if end < 0 else end
    if text.startswith("/*", i):
        return text.index("*/", i + 2) + 2
    if text[i] in "\"'":
        quote, j = text[i], i + 1
        while text[j] != quote:
            j += 2 if text[j] == "\\" else 1
        return j + 1
    return i


def matching(text, open_index, open_char="{", close_char="}"):
    depth, i = 0, open_index
    while True:
        skipped = skip_literal(text, i)
        if skipped != i:
            i = skipped
            continue
        if text[i] == open_char:
            depth += 1
        elif text[i] == close_char:
            depth -= 1
            if depth == 0:
                return i
        i += 1


def strip_comments(text):
    """Comments blanked out with the same length, so indices stay valid in the original."""
    out, i = [], 0
    while i < len(text):
        skipped = skip_literal(text, i)
        if skipped != i:
            if text[i] in "\"'":
                out.append(text[i:skipped])
            else:
                out.append(re.sub(r"[^\n]", " ", text[i:skipped]))
            i = skipped
        else:
            out.append(text[i])
            i += 1
    return "".join(out)


class Member:
    def __init__(self, annotations, modifiers, type_, name, initializer):
        self.annotations = annotations
        self.modifiers = modifiers
        self.type = type_
        self.name = name
        self.initializer = initializer

    @property
    def static(self):
        return "static" in self.modifiers

    @property
    def final(self):
        return "final" in self.modifiers

    @property
    def non_null(self):
        return any(a == "@NonNull" for a in self.annotations)

    def has(self, prefix):
        return any(a.startswith(prefix) for a in self.annotations)


def scan_body(body, class_name):
    """Fields, no-argument method names and whether an explicit constructor exists, at depth 0."""
    fields, methods, has_constructor = [], set(), False
    statement, i, paren = [], 0, 0
    while i < len(body):
        skipped = skip_literal(body, i)
        if skipped != i:
            if body[i] in "\"'":
                statement.append(body[i:skipped])
            i = skipped
            continue
        c = body[i]
        if c == "(":
            paren += 1
        elif c == ")":
            paren -= 1
        if c == "{" and paren == 0:
            text = "".join(statement)
            close = matching(body, i)
            if re.search(r"=", ANNOTATION.sub("", text)) and not re.search(r"\)\s*$", text.strip()):
                statement.append(body[i:close + 1])  # array initialiser or anonymous class
                i = close + 1
                continue
            header = ANNOTATION.sub(" ", text).strip()
            if re.match(rf"^(?:(?:public|protected|private)\s+)?{class_name}\s*\(", header):
                has_constructor = True
            m = re.search(r"(\w+)\s*\(\s*\)\s*(?:throws[^{]*)?$", header)
            if m and not re.search(r"\b(class|interface|enum|new)\b", header):
                methods.add(m.group(1))
            statement, i = [], close + 1
            continue
        if c == ";" and paren == 0:
            parse_field("".join(statement), fields, methods)
            statement = []
            i += 1
            continue
        statement.append(c)
        i += 1
    return fields, methods, has_constructor


def parse_field(text, fields, methods):
    declaration, _, initializer = text.partition("=")
    annotations = [a.replace(" ", "") for a in ANNOTATION.findall(declaration)]
    declaration = " ".join(ANNOTATION.sub(" ", declaration).split())
    if "(" in declaration:
        m = re.search(r"(\w+)\s*\(\s*\)$", declaration)  # abstract or interface method
        if m:
            methods.add(m.group(1))
        return
    m = re.match(r"^((?:(?:public|protected|private|static|final|transient|volatile)\s+)*)(.+?)\s+(\w+)$", declaration)
    if not m:
        return
    fields.append(Member(annotations, m.group(1).split(), m.group(2).strip(), m.group(3), initializer.strip() or None))


def null_checks(members, indent):
    return "".join(
        f'{indent}if ({m.name} == null) {{\n{indent}    throw new NullPointerException("{m.name} is marked non-null but is null");\n{indent}}}\n'
        for m in members if m.non_null and m.type not in PRIMITIVES)


def constructor(visibility, name, members, indent):
    params = ", ".join(f"{m.type} {m.name}" for m in members)
    body = null_checks(members, indent + "    ") + "".join(f"{indent}    this.{m.name} = {m.name};\n" for m in members)
    prefix = f"{visibility} " if visibility else ""
    return f"{indent}{prefix}{name}({params}) {{\n{body}{indent}}}\n"


def getter(m, indent):
    return f"{indent}public {m.type} {m.name}() {{\n{indent}    return this.{m.name};\n{indent}}}\n"


def equals_and_hash_code(name, generic, members, indent):
    raw = f"{name}<{', '.join('?' for _ in generic)}>" if generic else name
    i2, i3 = indent + "    ", indent + "        "
    compare, hashes = [], []
    for m in members:
        t, n = m.type, m.name
        if t == "double":
            compare.append(f"Double.compare(this.{n}, other.{n}) != 0")
            hashes.append(f"{i2}final long ${n} = Double.doubleToLongBits(this.{n});\n{i2}result = result * PRIME + (int) (${n} >>> 32 ^ ${n});\n")
        elif t == "float":
            compare.append(f"Float.compare(this.{n}, other.{n}) != 0")
            hashes.append(f"{i2}result = result * PRIME + Float.floatToIntBits(this.{n});\n")
        elif t == "boolean":
            compare.append(f"this.{n} != other.{n}")
            hashes.append(f"{i2}result = result * PRIME + (this.{n} ? 79 : 97);\n")
        elif t == "long":
            compare.append(f"this.{n} != other.{n}")
            hashes.append(f"{i2}result = result * PRIME + (int) (this.{n} >>> 32 ^ this.{n});\n")
        elif t in PRIMITIVES:
            compare.append(f"this.{n} != other.{n}")
            hashes.append(f"{i2}result = result * PRIME + this.{n};\n")
        elif t.endswith("[]"):
            deep = t[:-2] not in PRIMITIVES
            compare.append(f"!java.util.Arrays.{'deepEquals' if deep else 'equals'}(this.{n}, other.{n})")
            hashes.append(f"{i2}result = result * PRIME + java.util.Arrays.{'deepHashCode' if deep else 'hashCode'}(this.{n});\n")
        else:
            compare.append(f"!java.util.Objects.equals(this.{n}, other.{n})")
            hashes.append(f"{i2}result = result * PRIME + (this.{n} == null ? 43 : this.{n}.hashCode());\n")
    checks = "".join(f"{i2}if ({c}) {{\n{i3}return false;\n{i2}}}\n" for c in compare)
    return (
        f"{indent}@Override\n{indent}public boolean equals(Object o) {{\n"
        f"{i2}if (o == this) {{\n{i3}return true;\n{i2}}}\n"
        f"{i2}if (!(o instanceof {name})) {{\n{i3}return false;\n{i2}}}\n"
        f"{i2}final {raw} other = ({raw}) o;\n"
        f"{i2}if (!other.canEqual(this)) {{\n{i3}return false;\n{i2}}}\n"
        f"{checks}{i2}return true;\n{indent}}}\n\n"
        f"{indent}protected boolean canEqual(Object other) {{\n{i2}return other instanceof {name};\n{indent}}}\n\n"
        f"{indent}@Override\n{indent}public int hashCode() {{\n{i2}final int PRIME = 59;\n{i2}int result = 1;\n"
        f"{''.join(hashes)}{i2}return result;\n{indent}}}\n"
    )


def builder(name, members, indent):
    i2, i3 = indent + "    ", indent + "        "
    b = f"{name}Builder"
    parts = [constructor("", name, members, indent), "\n",
             f"{indent}public static {b} builder() {{\n{i2}return new {b}();\n{indent}}}\n\n",
             f"{indent}public static class {b} {{\n"]
    for m in members:
        parts.append(f"{i2}private {m.type} {m.name};\n")
        if m.has("@Builder.Default"):
            parts.append(f"{i2}private boolean {m.name}$set;\n")
    parts.append(f"\n{i2}{b}() {{\n{i2}}}\n")
    for m in members:
        flag = f"\n{i3}this.{m.name}$set = true;" if m.has("@Builder.Default") else ""
        parts.append(f"\n{i2}public {b} {m.name}({m.type} {m.name}) {{\n{i3}this.{m.name} = {m.name};{flag}\n{i3}return this;\n{i2}}}\n")
    args = ", ".join(f"this.{m.name}$set ? this.{m.name} : {name}.$default${m.name}()" if m.has("@Builder.Default") else f"this.{m.name}"
                     for m in members)
    parts.append(f"\n{i2}public {name} build() {{\n{i3}return new {name}({args});\n{i2}}}\n{indent}}}\n")
    for m in members:
        if m.has("@Builder.Default"):
            parts.append(f"\n{indent}private static {m.type} $default${m.name}() {{\n{i2}return {m.default};\n{indent}}}\n")
    return "".join(parts)


CLASS_ANNOTATIONS = ("Data", "Getter", "Builder", "RequiredArgsConstructor", "AllArgsConstructor", "NoArgsConstructor", "EqualsAndHashCode")


def process(source):
    """Returns the delomboked text of one file; classes are handled innermost first."""
    clean = strip_comments(source)
    edits = []  # (index, text to insert) and (start, end, replacement)
    replacements = []
    for m in re.finditer(r"(?:^[ \t]*@[^\n]*\n)+[ \t]*(?:(?:public|protected|private|abstract|static|final)\s+)*(class|interface|enum)\s+(\w+)(\s*<[^{]*?>)?[^{]*\{", clean, re.M):
        header = m.group(0)
        annotations = {a.split("(")[0].strip()[1:]: a for a in ANNOTATION.findall(header[:header.rfind(m.group(1))])}
        wanted = [a for a in CLASS_ANNOTATIONS if a in annotations]
        if not wanted:
            continue
        name = m.group(2)
        generic = [p.strip().split()[0] for p in re.split(r",(?![^<]*>)", m.group(3).strip()[1:-1])] if m.group(3) else []
        open_index = m.end() - 1
        close_index = matching(clean, open_index)
        body = clean[open_index + 1:close_index]
        fields, methods, has_constructor = scan_body(body, name)
        instance = [f for f in fields if not f.static]
        indent = re.match(r"[ \t]*", clean[clean.rfind("\n", 0, close_index) + 1:]).group(0) + "    "
        generated = []
        if "Builder" in annotations:
            for f in instance:
                if f.has("@Builder.Default"):
                    f.default = f.initializer
                    decl_match = re.compile(rf"\b{re.escape(f.name)}\s*=\s*{re.escape(f.initializer)}\s*;").search(clean, open_index)
                    replacements.append((decl_match.start(), decl_match.end(), f"{f.name};"))
            generated.append(builder(name, [f for f in instance if not (f.final and f.initializer and not f.has("@Builder.Default"))], indent))
        if "RequiredArgsConstructor" in annotations or ("Data" in annotations and not has_constructor):
            generated.append(constructor("public", name, [f for f in instance if (f.final and f.initializer is None) or (f.non_null and not f.final and f.initializer is None)], indent))
        if "AllArgsConstructor" in annotations:
            generated.append(constructor("public", name, [f for f in instance if not (f.final and f.initializer)], indent))
        if "NoArgsConstructor" in annotations:
            access = "private" if "PRIVATE" in annotations["NoArgsConstructor"] else "public"
            generated.append(f"{indent}{access} {name}() {{\n{indent}}}\n")
        if "Getter" in annotations or "Data" in annotations:
            for f in instance:
                if f.name not in methods and not f.has("@Getter(AccessLevel.NONE)"):
                    generated.append(getter(f, indent))
        if "Data" in annotations or "EqualsAndHashCode" in annotations:
            generated.append(equals_and_hash_code(name, generic, [f for f in instance if "transient" not in f.modifiers], indent))
        edits.append((close_index, "\n".join(generated)))
    # Field-level @Getter on classes without a class-level Getter/Data.
    for m in re.finditer(r"@Getter\s*\n(\s*)((?:(?:public|protected|private|static|final)\s+)*)([\w<>\[\], ?]+?)\s+(\w+)\s*(?:=[^;]*)?;", clean):
        owner_open = None
        for c in re.finditer(r"\bclass\s+(\w+)[^{;]*\{", clean[:m.start()]):
            if matching(clean, c.end() - 1) > m.start():
                owner_open = c
        owner_close = matching(clean, owner_open.end() - 1)
        _, methods, _ = scan_body(clean[owner_open.end():owner_close], owner_open.group(1))
        if m.group(4) in methods:
            continue
        indent = re.match(r"[ \t]*", clean[clean.rfind("\n", 0, owner_close) + 1:]).group(0) + "    "
        edits.append((owner_close, getter(Member([], m.group(2).split(), m.group(3).strip(), m.group(4), None), indent)))
    # @SneakyThrows(IOException.class) on a method whose callers are outside the oracle.
    for m in re.finditer(r"@SneakyThrows\(IOException\.class\)\s*\n\s*[^{]*\)\s*(?=\{)", clean):
        replacements.append((m.end(), m.end(), "throws IOException "))
    # Generated members go before the class's closing brace, separated by a blank line.
    changes = [(i, i, text, True) for i, text in edits] + [(start, end, text, False) for start, end, text in replacements]
    out = source
    for start, end, text, member in sorted(changes, key=lambda change: (change[0], change[1]), reverse=True):
        if member:
            line_start = out.rfind("\n", 0, start) + 1
            brace_indent = out[line_start:start]
            out = out[:line_start].rstrip() + "\n\n" + text + brace_indent + out[start:]
        else:
            out = out[:start] + text + out[end:]
    return out


def main():
    package_root = pathlib.Path(sys.argv[1])
    output = pathlib.Path(sys.argv[2])
    files = []
    for relative in SOURCES:
        path = package_root / relative
        if "*" in relative:
            files.extend(sorted(package_root.glob(relative)))
        else:
            files.extend(sorted(path.rglob("*.java")) if path.is_dir() else [path])
    chunks = []
    for path in files:
        original = path.read_text()
        if strip_comments(original).find("@") < 0:
            continue
        patched = process(original)
        if patched == original:
            continue
        relative = "net/buildtheearth/terraminusminus/" + path.relative_to(package_root).as_posix()
        diff = difflib.unified_diff(original.splitlines(keepends=True), patched.splitlines(keepends=True), f"a/{relative}", f"b/{relative}", n=3)
        # difflib leaves a final line without newline unmarked; patch(1) needs the marker.
        chunks.append("".join(line if line.endswith("\n") else line + "\n\\ No newline at end of file\n" for line in diff))
    output.write_text("".join(chunks))
    print(f"{output}: {len(chunks)} files")


if __name__ == "__main__":
    main()
