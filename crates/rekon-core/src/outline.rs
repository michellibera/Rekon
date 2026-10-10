//! Static outline of a source file: definitions (types, functions, modules, classes)
//! with their line ranges. Per-language patterns find where a definition starts,
//! braces (indentation for Python) find where it ends. Strings and comments are
//! blanked first, so they never count. Not a parser: unusual code is simply missed.
//! The outline names symbols across files and grounds the model's evidence.

use std::sync::OnceLock;

use regex::Regex;

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// `fn`, `struct`, `enum`, `trait`, `impl`, `mod`, `type`, `macro`, `class`,
    /// `interface`, `record`, `method`, `def`, ...
    pub kind: &'static str,
    /// Identifier as written (`run`, `Store`); for `impl` the implementing type.
    pub name: String,
    /// Name with its container, unique in most repositories: `init::run`,
    /// `Store::update`, `OrderService.create`, `impl Backend for ClaudeBackend`.
    pub qualified: String,
    /// 1-based, inclusive.
    pub start: u32,
    pub end: u32,
    /// Number of enclosing items.
    pub depth: usize,
}

impl Item {
    /// Types and other definitions worth naming in other files.
    pub fn is_type(&self) -> bool {
        matches!(
            self.kind,
            "struct" | "enum" | "trait" | "union" | "class" | "interface" | "record" | "object" | "protocol"
        )
    }
}

macro_rules! regex {
    ($pattern:expr) => {{
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new($pattern).expect("valid outline pattern"))
    }};
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lang {
    Rust,
    /// C, C++, C#, Java, Kotlin, Scala, Swift, Go: `'x'` is a character.
    CLike,
    /// JavaScript, TypeScript, PHP, Dart: `'...'` and `` `...` `` are strings.
    Js,
    Python,
}

fn lang(path: &str) -> Option<Lang> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit_once('.')?.1.to_lowercase();
    Some(match ext.as_str() {
        "rs" => Lang::Rust,
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "cs" | "java" | "kt" | "kts" | "scala" | "swift" | "go"
        | "groovy" | "gradle" => Lang::CLike,
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts" | "php" | "dart" => Lang::Js,
        "py" | "pyi" => Lang::Python,
        _ => return None,
    })
}

/// Outline of `text`, sorted by start line; empty for languages it does not know.
pub fn outline(path: &str, text: &str) -> Vec<Item> {
    let Some(lang) = lang(path) else {
        return Vec::new();
    };
    let blanked = blank(text, lang);
    let lines: Vec<&str> = blanked.lines().collect();
    let mut items = match lang {
        Lang::Rust => rust_items(&lines),
        Lang::Python => python_items(&lines),
        Lang::CLike | Lang::Js => brace_items(&lines, lang),
    };
    items.sort_by_key(|it| (it.start, std::cmp::Reverse(it.end)));
    nest(&mut items, lang, &rust_module(path));
    if lang == Lang::Rust {
        drop_test_modules(&mut items);
    }
    items
}

/// Rust module of a file: the file stem, the folder for `mod.rs`, the crate folder
/// (with `_`) for `lib.rs` and `main.rs`.
fn rust_module(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    let file = parts.last().copied().unwrap_or("");
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    let parent = |up: usize| parts.len().checked_sub(up + 1).map(|i| parts[i]);
    let name = match stem {
        "mod" => parent(1).unwrap_or(stem),
        "lib" | "main" => match parent(1) {
            Some("src") => parent(2).unwrap_or(stem),
            Some(p) => p,
            None => stem,
        },
        _ => stem,
    };
    name.replace('-', "_")
}

// ----- blanking -----

/// `text` with string, character and comment contents replaced by spaces; line
/// breaks stay, so line numbers do not change.
fn blank(text: &str, lang: Lang) -> String {
    let b: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let blank_to = |out: &mut String, from: usize, to: usize| {
        for &c in &b[from..to.min(b.len())] {
            out.push(if c == '\n' { '\n' } else { ' ' });
        }
    };
    while i < b.len() {
        let c = b[i];
        let next = b.get(i + 1).copied();
        let line_comment = match lang {
            Lang::Python => c == '#',
            _ => c == '/' && next == Some('/'),
        };
        if line_comment {
            let end = (i..b.len()).find(|&j| b[j] == '\n').unwrap_or(b.len());
            blank_to(&mut out, i, end);
            i = end;
            continue;
        }
        if lang != Lang::Python && c == '/' && next == Some('*') {
            let end = (i + 2..b.len().saturating_sub(1))
                .find(|&j| b[j] == '*' && b[j + 1] == '/')
                .map_or(b.len(), |j| j + 2);
            blank_to(&mut out, i, end);
            i = end;
            continue;
        }
        if lang == Lang::Rust && c == 'r' && !prev_ident(&b, i) {
            let hashes = b[i + 1..].iter().take_while(|&&h| h == '#').count();
            if b.get(i + 1 + hashes) == Some(&'"') {
                let body = i + 2 + hashes;
                let end = (body..b.len())
                    .find(|&j| b[j] == '"' && (1..=hashes).all(|k| b.get(j + k) == Some(&'#')))
                    .map_or(b.len(), |j| j + 1 + hashes);
                blank_to(&mut out, i, end);
                i = end;
                continue;
            }
        }
        if lang == Lang::Python && (c == '"' || c == '\'') && next == Some(c) && b.get(i + 2) == Some(&c) {
            let end = (i + 3..b.len().saturating_sub(2))
                .find(|&j| b[j] == c && b[j + 1] == c && b[j + 2] == c)
                .map_or(b.len(), |j| j + 3);
            blank_to(&mut out, i, end);
            i = end;
            continue;
        }
        let string_quote = c == '"'
            || (c == '\'' && matches!(lang, Lang::Js | Lang::Python))
            || (c == '`' && (lang == Lang::Js || lang == Lang::CLike));
        if string_quote {
            let mut j = i + 1;
            while j < b.len() && b[j] != c {
                if b[j] == '\\' {
                    j += 1;
                } else if b[j] == '\n' && c != '`' && lang == Lang::Python {
                    break;
                }
                j += 1;
            }
            let end = (j + 1).min(b.len());
            blank_to(&mut out, i, end);
            i = end;
            continue;
        }
        if c == '\'' {
            // A character literal ('x', '\n', '\u{1F600}'); otherwise a Rust lifetime.
            let end = if next == Some('\\') {
                (i + 2..(i + 12).min(b.len())).find(|&j| b[j] == '\'').map(|j| j + 1)
            } else if b.get(i + 2) == Some(&'\'') {
                Some(i + 3)
            } else {
                None
            };
            if let Some(end) = end {
                blank_to(&mut out, i, end);
                i = end;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn prev_ident(b: &[char], i: usize) -> bool {
    i > 0 && (b[i - 1].is_alphanumeric() || b[i - 1] == '_') && b[i - 1] != 'b'
}

// ----- ends -----

/// Last line (0-based) of a definition starting at `(line, col)` of the blanked
/// lines: the brace matching its first `{`, or a `;` that comes before any brace.
/// Without either, a blank line ends the signature.
fn find_end(lines: &[&str], line: usize, col: usize) -> usize {
    let mut paren = 0i32;
    let mut depth = 0i32;
    let mut opened = false;
    for (i, l) in lines.iter().enumerate().skip(line) {
        if !opened && i > line {
            if paren <= 0 && l.trim().is_empty() {
                return (line..i).rev().find(|&k| !lines[k].trim().is_empty()).unwrap_or(line);
            }
            if i > line + 300 {
                return line;
            }
        }
        let from = if i == line { col.min(l.len()) } else { 0 };
        for ch in l.get(from..).unwrap_or("").chars() {
            match ch {
                '(' | '[' if !opened => paren += 1,
                ')' | ']' if !opened => paren -= 1,
                '{' if opened || paren <= 0 => {
                    depth += 1;
                    opened = true;
                }
                '}' if opened => {
                    depth -= 1;
                    if depth == 0 {
                        return i;
                    }
                }
                ';' if !opened && paren <= 0 => return i,
                _ => {}
            }
        }
    }
    lines.len().saturating_sub(1).max(line)
}

fn item(kind: &'static str, name: &str, line: usize, end: usize) -> Item {
    Item {
        kind,
        name: name.to_string(),
        qualified: name.to_string(),
        start: line as u32 + 1,
        end: end as u32 + 1,
        depth: 0,
    }
}

// ----- Rust -----

fn rust_items(lines: &[&str]) -> Vec<Item> {
    let fn_re =
        regex!(r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:(?:default|async|const|unsafe|extern)\s+)*fn\s+([A-Za-z_]\w*)");
    let ty_re =
        regex!(r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:unsafe\s+)?(struct|enum|trait|union|mod|type)\s+([A-Za-z_]\w*)");
    let impl_re = regex!(r"^\s*(?:unsafe\s+)?impl\b");
    let macro_re = regex!(r"^\s*macro_rules!\s*([A-Za-z_]\w*)");
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some(c) = fn_re.captures(line) {
            let end = find_end(lines, i, c.get(0).map_or(0, |m| m.end()));
            out.push(item("fn", &c[1], i, end));
        } else if let Some(c) = ty_re.captures(line) {
            let kind = match &c[1] {
                "struct" => "struct",
                "enum" => "enum",
                "trait" => "trait",
                "union" => "union",
                "mod" => "mod",
                _ => "type",
            };
            let end = find_end(lines, i, c.get(0).map_or(0, |m| m.end()));
            out.push(item(kind, &c[2], i, end));
        } else if let Some(m) = impl_re.find(line) {
            let Some((name, header)) = impl_header(&line[m.end()..]) else {
                continue;
            };
            let end = find_end(lines, i, m.end());
            let mut it = item("impl", &name, i, end);
            it.qualified = format!("impl {header}");
            out.push(it);
        } else if let Some(c) = macro_re.captures(line) {
            let end = find_end(lines, i, c.get(0).map_or(0, |m| m.end()));
            out.push(item("macro", &c[1], i, end));
        }
    }
    out
}

/// `<T> Trait for Type<T> where …` → (`Type`, `Trait for Type<T>`).
fn impl_header(rest: &str) -> Option<(String, String)> {
    let mut s = rest.trim_start();
    if s.starts_with('<') {
        let mut depth = 0;
        let cut = s.char_indices().find_map(|(i, c)| {
            match c {
                '<' => depth += 1,
                '>' => depth -= 1,
                _ => {}
            }
            (depth == 0).then_some(i + 1)
        })?;
        s = &s[cut..];
    }
    let header = s.split(['{', ';']).next()?.split(" where").next()?.trim();
    if header.is_empty() {
        return None;
    }
    let self_type = header.rsplit(" for ").next()?.trim();
    let base = self_type.trim_start_matches('&').trim_start_matches("dyn ").trim();
    let base = base.split('<').next()?;
    let name = base.rsplit("::").next()?.trim().to_string();
    (!name.is_empty()).then(|| (name, header.to_string()))
}

/// First line of a Rust test module (`#[cfg(test)] mod tests`) that ends the file:
/// the code before it can be analyzed without the tests.
pub fn tests_start(path: &str, text: &str) -> Option<u32> {
    if lang(path) != Some(Lang::Rust) {
        return None;
    }
    let blanked = blank(text, Lang::Rust);
    let lines: Vec<&str> = blanked.lines().collect();
    let m = rust_items(&lines)
        .into_iter()
        .find(|it| it.kind == "mod" && matches!(it.name.as_str(), "tests" | "test") && it.end > it.start)?;
    if !lines[m.end as usize..].iter().all(|l| l.trim().is_empty()) {
        return None;
    }
    let above = m.start as usize - 1;
    let attribute = above > 0 && lines[above - 1].trim_start().starts_with("#[cfg(test)]");
    Some(if attribute { m.start - 1 } else { m.start })
}

fn drop_test_modules(items: &mut Vec<Item>) {
    let tests: Vec<(u32, u32)> = items
        .iter()
        .filter(|it| it.kind == "mod" && matches!(it.name.as_str(), "tests" | "test") && it.end > it.start)
        .map(|it| (it.start, it.end))
        .collect();
    items.retain(|it| !tests.iter().any(|&(s, e)| it.start >= s && it.end <= e));
}

// ----- brace languages -----

fn brace_items(lines: &[&str], lang: Lang) -> Vec<Item> {
    let class_re = regex!(
        r"^\s*(?:@\w+(?:\([^)]*\))?\s+)*(?:(?:export|default|public|private|protected|internal|static|abstract|sealed|partial|final|open|data|inline|value|enum|declare|readonly)\s+)*(class|interface|struct|enum|record|trait|object|protocol)\s+([A-Za-z_$][\w$]*)"
    );
    let func_re = regex!(
        r"^\s*(?:(?:export|default|public|private|protected|internal|static|abstract|final|async|override|open|suspend|inline|operator)\s+)*(?:func|fun|function)\s*\*?\s*(?:<[^>]*>\s*)?(?:[A-Za-z_$][\w$]*\.)?([A-Za-z_$][\w$]*)\s*[(<]"
    );
    let arrow_re = regex!(
        r"^\s*(?:export\s+)?(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=]+)?=\s*(?:async\s+)?(?:\([^)]*\)|[A-Za-z_$][\w$]*)\s*(?::\s*[^=]+)?=>"
    );
    let type_re = regex!(r"^\s*(?:export\s+)?(?:declare\s+)?type\s+([A-Za-z_$][\w$]*)\s*(?:<[^=]*>)?\s*=");
    let go_func_re = regex!(r"^func\s+(?:\(\s*\w*\s*\*?\s*(\w+)[^)]*\)\s*)?([A-Za-z_]\w*)\s*[(\[]");
    let go_type_re = regex!(r"^type\s+([A-Za-z_]\w*)\s+(struct|interface)\b");
    let method_re = regex!(
        r"^\s*(?:@\w+(?:\([^)]*\))?\s+)*(?:(?:public|private|protected|internal|static|virtual|override|abstract|async|final|sealed|synchronized|extern|unsafe|new|readonly|get|set)\s+)*(?:[\w<>\[\],.?]+\s+)??([A-Za-z_$][\w$]*)\s*(?:<[^>()]*>)?\s*\("
    );
    const NOT_METHODS: &[&str] = &[
        "if", "for", "while", "switch", "catch", "return", "function", "new", "else", "do", "try", "typeof", "await",
        "super", "this", "throw", "using", "lock", "foreach", "fixed", "sizeof", "nameof", "yield", "when", "in", "of",
        "delete", "void", "base",
    ];
    let depth_at = brace_depths(lines);
    let mut out: Vec<Item> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let col_end = |m: regex::Match| m.end();
        if let Some(c) = go_type_re.captures(line) {
            let end = find_end(lines, i, col_end(c.get(0).unwrap()));
            out.push(item(
                if &c[2] == "struct" { "struct" } else { "interface" },
                &c[1],
                i,
                end,
            ));
        } else if let Some(c) = go_func_re.captures(line) {
            let end = find_end(lines, i, col_end(c.get(0).unwrap()));
            let mut it = item("fn", &c[2], i, end);
            if let Some(recv) = c.get(1) {
                it.kind = "method";
                it.qualified = format!("{}.{}", recv.as_str(), &c[2]);
            }
            out.push(it);
        } else if let Some(c) = class_re.captures(line) {
            let kind = match &c[1] {
                "class" => "class",
                "interface" => "interface",
                "struct" => "struct",
                "enum" => "enum",
                "record" => "record",
                "trait" => "trait",
                "object" => "object",
                _ => "protocol",
            };
            let end = find_end(lines, i, col_end(c.get(0).unwrap()));
            out.push(item(kind, &c[2], i, end));
        } else if let Some(c) = func_re.captures(line) {
            let end = find_end(lines, i, col_end(c.get(0).unwrap()));
            out.push(item("fn", &c[1], i, end));
        } else if let Some(c) = arrow_re.captures(line) {
            let end = find_end(lines, i, col_end(c.get(0).unwrap()));
            out.push(item("fn", &c[1], i, end));
        } else if lang == Lang::Js
            && let Some(c) = type_re.captures(line)
        {
            let end = find_end(lines, i, col_end(c.get(0).unwrap()));
            out.push(item("type", &c[1], i, end));
        } else if let Some(c) = method_re.captures(line) {
            // A member only directly inside a class-like body.
            let name = &c[1];
            if NOT_METHODS.contains(&name) {
                continue;
            }
            let owner = out.iter().rev().find(|o| {
                matches!(o.kind, "class" | "struct" | "record" | "trait" | "object" | "interface")
                    && o.start as usize - 1 < i
                    && i < o.end as usize
                    && depth_at[i] == depth_at[o.start as usize - 1] + 1
            });
            if owner.is_some() {
                let end = find_end(lines, i, col_end(c.get(0).unwrap()));
                out.push(item("method", name, i, end));
            }
        }
    }
    out
}

/// Brace depth at the start of every line.
fn brace_depths(lines: &[&str]) -> Vec<i32> {
    let mut depth = 0;
    lines
        .iter()
        .map(|l| {
            let at = depth;
            for c in l.chars() {
                match c {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
            }
            at
        })
        .collect()
}

// ----- Python -----

fn python_items(lines: &[&str]) -> Vec<Item> {
    let def_re = regex!(r"^(\s*)(?:async\s+)?(def|class)\s+([A-Za-z_]\w*)");
    let indent = |l: &str| l.len() - l.trim_start().len();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(c) = def_re.captures(line) else { continue };
        let own = c[1].len();
        // The signature ends at the first `:` outside brackets.
        let mut paren = 0i32;
        let mut sig_end = None;
        'sig: for (j, l) in lines.iter().enumerate().skip(i).take(60) {
            let from = if j == i { c.get(0).map_or(0, |m| m.end()) } else { 0 };
            for ch in l.get(from..).unwrap_or("").chars() {
                match ch {
                    '(' | '[' | '{' => paren += 1,
                    ')' | ']' | '}' => paren -= 1,
                    ':' if paren <= 0 => {
                        sig_end = Some(j);
                        break 'sig;
                    }
                    _ => {}
                }
            }
        }
        let sig_end = sig_end.unwrap_or(i);
        let mut end = sig_end;
        for (k, l) in lines.iter().enumerate().skip(sig_end + 1) {
            if l.trim().is_empty() {
                continue;
            }
            if indent(l) <= own {
                break;
            }
            end = k;
        }
        let kind = if &c[2] == "class" { "class" } else { "def" };
        out.push(item(kind, &c[3], i, end));
    }
    out
}

// ----- nesting -----

/// Sets depths and qualified names from the enclosing items (items sorted by start).
fn nest(items: &mut [Item], lang: Lang, module: &str) {
    let sep = if lang == Lang::Rust { "::" } else { "." };
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..items.len() {
        while let Some(&top) = stack.last() {
            if items[top].end >= items[i].end && items[top].start <= items[i].start && top != i {
                break;
            }
            stack.pop();
        }
        items[i].depth = stack.len();
        let parent = stack.last().map(|&p| items[p].name.clone());
        let qualified = match (parent, items[i].kind) {
            (_, "impl") => None,
            (Some(pname), _) => Some(format!("{pname}{sep}{}", items[i].name)),
            (None, "fn") if lang == Lang::Rust => Some(format!("{module}::{}", items[i].name)),
            (None, _) => None,
        };
        if let Some(q) = qualified
            && !items[i].qualified.contains('.')
        {
            items[i].qualified = q;
        }
        stack.push(i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn short(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|it| format!("{} {} {}-{} d{}", it.kind, it.qualified, it.start, it.end, it.depth))
            .collect()
    }

    #[test]
    fn rust_items_with_ranges_and_containers() {
        let src = r##"//! Docs { not a brace
use std::fmt;

pub struct Store {
    dir: String, // }
}

impl<'a, T: Clone> Store {
    pub fn new(dir: &str) -> Self {
        let s = "}{";
        let r = r#"{"#;
        let c = '}';
        Self { dir: dir.into() }
    }

    fn lifetime<'b>(&'b self) -> &'b str {
        &self.dir
    }
}

impl fmt::Display for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.dir)
    }
}

pub(crate) async fn run(
    a: u32,
) -> u32 {
    a
}

mod inner;
pub trait Backend: Send {
    fn ask(&self) -> u32;
}

#[cfg(test)]
mod tests {
    fn helper() {}
}
"##;
        let items = outline("crates/rekon-core/src/store.rs", src);
        assert_eq!(tests_start("src/store.rs", src), Some(38));
        assert_eq!(tests_start("src/store.ts", src), None);
        assert_eq!(
            tests_start(
                "src/a.rs",
                "mod tests {
}
fn after() {}
"
            ),
            None
        );
        assert_eq!(
            short(&items),
            [
                "struct Store 4-6 d0",
                "impl impl Store 8-19 d0",
                "fn Store::new 9-14 d1",
                "fn Store::lifetime 16-18 d1",
                "impl impl fmt::Display for Store 21-25 d0",
                "fn Store::fmt 22-24 d1",
                "fn store::run 27-31 d0",
                "mod inner 33-33 d0",
                "trait Backend 34-36 d0",
                "fn Backend::ask 35-35 d1",
            ]
        );
    }

    #[test]
    fn module_names() {
        assert_eq!(rust_module("crates/rekon-core/src/init.rs"), "init");
        assert_eq!(rust_module("crates/rekon-core/src/backend/mod.rs"), "backend");
        assert_eq!(rust_module("crates/rekon-core/src/lib.rs"), "rekon_core");
        assert_eq!(rust_module("src/main.rs"), "main");
        assert_eq!(rust_module("main.rs"), "main");
    }

    #[test]
    fn typescript_classes_methods_and_functions() {
        let src = r#"import { x } from './x';

@Injectable()
export class OrderService {
  constructor(private readonly repo: Repo) {}

  async create(dto: CreateOrderDto): Promise<Order> {
    if (dto.items.length === 0) {
      throw new Error('empty {');
    }
    return this.repo.save(dto);
  }
}

export interface Order {
  id: string;
}

export const handler = async (event: Event) => {
  return 1;
};

export function helper(a: number): number {
  return a;
}

type Id = string;
"#;
        let items = outline("services/order.service.ts", src);
        assert_eq!(
            short(&items),
            [
                "class OrderService 4-13 d0",
                "method OrderService.constructor 5-5 d1",
                "method OrderService.create 7-12 d1",
                "interface Order 15-17 d0",
                "fn handler 19-21 d0",
                "fn helper 23-25 d0",
                "type Id 27-27 d0",
            ]
        );
    }

    #[test]
    fn csharp_and_go() {
        let cs = r#"namespace Shop {
    public sealed class PaymentService : IPaymentService {
        private readonly Stripe _stripe;

        public async Task<Result> PayAsync(Order order) {
            if (order == null) { return null; }
            return await _stripe.Charge(order);
        }
    }
}
"#;
        let items = outline("Shop/PaymentService.cs", cs);
        assert_eq!(
            short(&items),
            ["class PaymentService 2-9 d0", "method PaymentService.PayAsync 5-8 d1"]
        );
        let go = "package main\n\ntype Server struct {\n\tport int\n}\n\nfunc (s *Server) Start() error {\n\treturn nil\n}\n";
        let items = outline("cmd/server.go", go);
        assert_eq!(short(&items), ["struct Server 3-5 d0", "method Server.Start 7-9 d0"]);
    }

    #[test]
    fn python_by_indentation() {
        let src = "class Repo:\n    \"\"\"Docs:\n    def not_this(): pass\n    \"\"\"\n\n    def save(\n        self, x\n    ):\n        return x\n\n\ndef main():\n    pass\n";
        let items = outline("app/repo.py", src);
        assert_eq!(
            short(&items),
            ["class Repo 1-9 d0", "def Repo.save 6-9 d1", "def main 12-13 d0"]
        );
    }

    #[test]
    fn unknown_languages_have_no_outline() {
        assert!(outline("README.md", "# x").is_empty());
        assert!(outline("Makefile", "all:\n\tx").is_empty());
    }
}
