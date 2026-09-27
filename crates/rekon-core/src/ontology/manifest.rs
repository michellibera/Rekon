//! Packages and their external dependencies, read from manifests together with the
//! lines that declare them: Cargo.toml, package.json, *.csproj, go.mod,
//! pyproject.toml and requirements.txt. Line-based reading, no full TOML/XML parser.

use std::path::Path;

use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct Dep {
    pub name: String,
    /// Manifest and line that declare the dependency.
    pub manifest: String,
    pub line: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Package {
    pub name: String,
    /// Folder of the manifest (`""` for the repository root).
    pub dir: String,
    pub manifest: String,
    /// Lines that declare the package (e.g. the `[package]` section).
    pub lines: (u32, u32),
    pub deps: Vec<Dep>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Manifests {
    pub packages: Vec<Package>,
    /// Versions declared once for a whole workspace (Cargo `[workspace.dependencies]`).
    pub shared: Vec<Dep>,
    /// Dependencies of a folder without a package name (requirements.txt): (folder, dep).
    pub loose: Vec<(String, Dep)>,
    /// Manifest and lines that describe the workspace (Cargo `[workspace]`).
    pub workspace: Option<(String, (u32, u32))>,
}

impl Manifests {
    pub fn is_package(&self, name: &str) -> bool {
        let key = normalize(name);
        self.packages.iter().any(|p| normalize(&p.name) == key)
    }
}

/// Dependency names differ only by `-`/`_` and case between ecosystems' spellings.
fn normalize(name: &str) -> String {
    name.to_lowercase().replace('_', "-")
}

/// Reads every manifest among the repository `files`.
pub fn read(root: &Path, files: &[String]) -> Manifests {
    let mut out = Manifests::default();
    for path in files {
        let name = path.rsplit('/').next().unwrap_or(path);
        let kind = match name {
            "Cargo.toml" => cargo,
            "package.json" => npm,
            "go.mod" => gomod,
            "pyproject.toml" => pyproject,
            "requirements.txt" => requirements,
            n if n.ends_with(".csproj") => csproj,
            _ => continue,
        };
        if path.split('/').any(|p| p == "node_modules" || p == "vendor") {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(root.join(path)) {
            kind(path, &text, &mut out);
        }
    }
    out
}

fn dir_of(path: &str) -> String {
    path.rsplit_once('/').map_or(String::new(), |(d, _)| d.to_string())
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches(|c| c == '"' || c == '\'').trim().to_string()
}

/// `#` comment removed, when outside a string.
fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    for (i, c) in line.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            (None, '#') => return &line[..i],
            _ => {}
        }
    }
    line
}

/// Opened minus closed brackets and braces outside strings.
fn bracket_delta(s: &str) -> i32 {
    let mut quote = None;
    let mut d = 0;
    for c in s.chars() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            (None, '[' | '{') => d += 1,
            (None, ']' | '}') => d -= 1,
            _ => {}
        }
    }
    d
}

/// Line-based TOML reading: `(line number, section, key, value)` for `key = value`
/// lines; multi-line values are skipped after their first line. Section header lines
/// are reported with an empty key.
fn toml_entries(text: &str) -> Vec<(u32, String, String, String)> {
    let mut out = Vec::new();
    let mut section = String::new();
    let mut depth = 0;
    for (i, raw) in text.lines().enumerate() {
        let nr = i as u32 + 1;
        let line = strip_comment(raw).trim();
        if depth > 0 {
            depth += bracket_delta(line);
            continue;
        }
        if line.starts_with('[') {
            section = line.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            out.push((nr, section.clone(), String::new(), String::new()));
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        depth = bracket_delta(value).max(0);
        out.push((nr, section.clone(), key.trim().to_string(), value.trim().to_string()));
    }
    out
}

/// Line range of `section` (header to its last non-empty line).
fn section_lines(text: &str, section: &str) -> Option<(u32, u32)> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|l| strip_comment(l).trim().trim_matches(|c| c == '[' || c == ']').trim() == section)?;
    let mut end = start;
    for (i, l) in lines.iter().enumerate().skip(start + 1) {
        let t = strip_comment(l).trim();
        if t.starts_with('[') {
            break;
        }
        if !t.is_empty() {
            end = i;
        }
    }
    Some((start as u32 + 1, end as u32 + 1))
}

fn cargo(path: &str, text: &str, out: &mut Manifests) {
    let mut name = None;
    let mut deps = Vec::new();
    let dep = |key: &str, line: u32| Dep {
        name: unquote(key.split('.').next().unwrap_or(key)),
        manifest: path.to_string(),
        line,
    };
    for (nr, section, key, value) in toml_entries(text) {
        let is_deps =
            section == "dependencies" || (section.starts_with("target.") && section.ends_with(".dependencies"));
        if key.is_empty() {
            if let Some(d) = section.strip_prefix("dependencies.") {
                deps.push(dep(d, nr));
            } else if let Some(d) = section.strip_prefix("workspace.dependencies.") {
                out.shared.push(dep(d, nr));
            }
            continue;
        }
        match section.as_str() {
            "package" if key == "name" => name = Some(unquote(&value)),
            "workspace.dependencies" => out.shared.push(dep(&key, nr)),
            _ if is_deps => deps.push(dep(&key, nr)),
            _ => {}
        }
    }
    if let Some(lines) = section_lines(text, "workspace") {
        out.workspace = Some((path.to_string(), lines));
    }
    if let Some(name) = name {
        out.packages.push(Package {
            name,
            dir: dir_of(path),
            manifest: path.to_string(),
            lines: section_lines(text, "package").unwrap_or((1, 1)),
            deps,
        });
    }
}

fn pyproject(path: &str, text: &str, out: &mut Manifests) {
    let mut name = None;
    let mut deps = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    for (nr, section, key, value) in toml_entries(text) {
        match (section.as_str(), key.as_str()) {
            ("project" | "tool.poetry", "name") => name = Some(unquote(&value)),
            ("project", "dependencies") => {
                // One requirement per line of the array.
                let mut depth = 0;
                for (i, l) in lines.iter().enumerate().skip(nr as usize - 1) {
                    depth += bracket_delta(l);
                    for part in l.split(',') {
                        let part = part
                            .trim()
                            .trim_start_matches("dependencies")
                            .trim_start_matches(['=', ' ', '[']);
                        if part.starts_with('"') || part.starts_with('\'') {
                            let req = requirement_name(&unquote(part));
                            if !req.is_empty() {
                                deps.push(Dep {
                                    name: req,
                                    manifest: path.to_string(),
                                    line: i as u32 + 1,
                                });
                            }
                        }
                    }
                    if depth <= 0 {
                        break;
                    }
                }
            }
            ("tool.poetry.dependencies", k) if !k.is_empty() && k != "python" => deps.push(Dep {
                name: unquote(k),
                manifest: path.to_string(),
                line: nr,
            }),
            _ => {}
        }
    }
    if let Some(name) = name {
        let lines = section_lines(text, "project")
            .or_else(|| section_lines(text, "tool.poetry"))
            .unwrap_or((1, 1));
        out.packages.push(Package {
            name,
            dir: dir_of(path),
            manifest: path.to_string(),
            lines,
            deps,
        });
    }
}

/// `requests>=2.0 ; python_version < "3.8"` → `requests`.
fn requirement_name(req: &str) -> String {
    req.split(|c: char| "<>=!~;[ @".contains(c))
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

fn requirements(path: &str, text: &str, out: &mut Manifests) {
    for (i, l) in text.lines().enumerate() {
        let l = strip_comment(l).trim();
        if l.is_empty() || l.starts_with('-') {
            continue;
        }
        let name = requirement_name(l);
        if !name.is_empty() {
            out.loose.push((
                dir_of(path),
                Dep {
                    name,
                    manifest: path.to_string(),
                    line: i as u32 + 1,
                },
            ));
        }
    }
}

fn npm(path: &str, text: &str, out: &mut Manifests) {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return;
    };
    let lines: Vec<&str> = text.lines().collect();
    let find = |needle: &str, from: usize| {
        lines
            .iter()
            .enumerate()
            .skip(from)
            .find(|(_, l)| l.contains(needle))
            .map(|(i, _)| i)
    };
    let dir = dir_of(path);
    let name = v
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| dir.rsplit('/').next().unwrap_or("package").to_string());
    let name_line = find("\"name\"", 0).map_or(1, |i| i as u32 + 1);
    let mut deps = Vec::new();
    if let Some(map) = v.get("dependencies").and_then(Value::as_object) {
        let start = find("\"dependencies\"", 0).unwrap_or(0);
        for key in map.keys() {
            let line = find(&format!("\"{key}\""), start).map_or(start as u32 + 1, |i| i as u32 + 1);
            deps.push(Dep {
                name: key.clone(),
                manifest: path.to_string(),
                line,
            });
        }
    }
    out.packages.push(Package {
        name,
        dir,
        manifest: path.to_string(),
        lines: (name_line, name_line),
        deps,
    });
}

/// Value of `attr="…"` in an XML line.
fn xml_attr(line: &str, attr: &str) -> Option<String> {
    let start = line.find(&format!("{attr}=\""))? + attr.len() + 2;
    let len = line[start..].find('"')?;
    Some(line[start..start + len].to_string())
}

fn csproj(path: &str, text: &str, out: &mut Manifests) {
    let file = path.rsplit('/').next().unwrap_or(path);
    let name = file.trim_end_matches(".csproj").to_string();
    let mut deps = Vec::new();
    let mut project_line = None;
    for (i, l) in text.lines().enumerate() {
        let nr = i as u32 + 1;
        if project_line.is_none() && (l.contains("<Project ") || l.contains("<Project>")) {
            project_line = Some(nr);
        }
        let dep = if l.contains("<PackageReference") {
            xml_attr(l, "Include")
        } else if l.contains("<ProjectReference") {
            // `..\Shop.Core\Shop.Core.csproj` → `Shop.Core`
            xml_attr(l, "Include").map(|p| {
                let f = p.rsplit(['/', '\\']).next().unwrap_or(&p).to_string();
                f.trim_end_matches(".csproj").to_string()
            })
        } else {
            None
        };
        if let Some(d) = dep {
            deps.push(Dep {
                name: d,
                manifest: path.to_string(),
                line: nr,
            });
        }
    }
    let line = project_line.unwrap_or(1);
    out.packages.push(Package {
        name,
        dir: dir_of(path),
        manifest: path.to_string(),
        lines: (line, line),
        deps,
    });
}

fn gomod(path: &str, text: &str, out: &mut Manifests) {
    let mut name = None;
    let mut name_line = 1;
    let mut deps = Vec::new();
    let mut in_block = false;
    for (i, l) in text.lines().enumerate() {
        let nr = i as u32 + 1;
        let t = l.split("//").next().unwrap_or("").trim();
        if let Some(m) = t.strip_prefix("module ") {
            name = Some(m.trim().rsplit('/').next().unwrap_or(m).to_string());
            name_line = nr;
        } else if t.starts_with("require (") {
            in_block = true;
        } else if in_block && t.starts_with(')') {
            in_block = false;
        } else if let Some(dep) = t.strip_prefix("require ").or(in_block.then_some(t))
            && let Some(module) = dep.split_whitespace().next()
        {
            deps.push(Dep {
                name: module.to_string(),
                manifest: path.to_string(),
                line: nr,
            });
        }
    }
    if let Some(name) = name {
        out.packages.push(Package {
            name,
            dir: dir_of(path),
            manifest: path.to_string(),
            lines: (name_line, name_line),
            deps,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all(files: &[(&str, &str)]) -> Manifests {
        let dir = tempfile::tempdir().unwrap();
        for (p, t) in files {
            let path = dir.path().join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, t).unwrap();
        }
        let names: Vec<String> = files.iter().map(|(p, _)| p.to_string()).collect();
        read(dir.path(), &names)
    }

    #[test]
    fn cargo_workspace_and_members() {
        let m = read_all(&[
            (
                "Cargo.toml",
                "[workspace]\nresolver = \"2\"\nmembers = [\n  \"crates/core\",\n]\n\n[workspace.dependencies]\ncore = { path = \"crates/core\" }\nserde = { version = \"1\", features = [\n  \"derive\",\n] }\nanyhow = \"1\" # errors\n",
            ),
            (
                "crates/core/Cargo.toml",
                "[package]\nname = \"core\"\nversion.workspace = true\n\n[dependencies]\nserde.workspace = true\n\"wait-timeout\" = \"0.2\"\n\n[dev-dependencies]\ntempfile = \"3\"\n\n[dependencies.blake3]\nversion = \"1\"\n",
            ),
        ]);
        assert_eq!(m.workspace, Some(("Cargo.toml".to_string(), (1, 5))));
        let shared: Vec<_> = m.shared.iter().map(|d| (d.name.as_str(), d.line)).collect();
        assert_eq!(shared, [("core", 8), ("serde", 9), ("anyhow", 12)]);
        assert_eq!(m.packages.len(), 1);
        let p = &m.packages[0];
        assert_eq!(
            (p.name.as_str(), p.dir.as_str(), p.lines),
            ("core", "crates/core", (1, 3))
        );
        let deps: Vec<_> = p.deps.iter().map(|d| (d.name.as_str(), d.line)).collect();
        assert_eq!(deps, [("serde", 6), ("wait-timeout", 7), ("blake3", 12)]);
        assert!(m.is_package("core"));
    }

    #[test]
    fn npm_csproj_go_python() {
        let m = read_all(&[
            (
                "web/package.json",
                "{\n  \"name\": \"shop-web\",\n  \"dependencies\": {\n    \"@angular/core\": \"^17\",\n    \"rxjs\": \"^7\"\n  },\n  \"devDependencies\": {\"jest\": \"1\"}\n}\n",
            ),
            (
                "api/Shop.Api.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk.Web\">\n  <ItemGroup>\n    <PackageReference Include=\"Stripe.net\" Version=\"43\" />\n    <ProjectReference Include=\"..\\Shop.Core\\Shop.Core.csproj\" />\n  </ItemGroup>\n</Project>\n",
            ),
            (
                "svc/go.mod",
                "module github.com/acme/orders\n\nrequire (\n\tgithub.com/lib/pq v1.10.9\n)\nrequire golang.org/x/sync v0.1.0\n",
            ),
            (
                "py/pyproject.toml",
                "[project]\nname = \"worker\"\ndependencies = [\n  \"requests>=2\",\n  \"pydantic[email]==2.5\",\n]\n",
            ),
            ("tools/requirements.txt", "# pinned\nclick==8.1\n-r base.txt\n"),
        ]);
        let pkg = |n: &str| m.packages.iter().find(|p| p.name == n).unwrap();
        let deps = |n: &str| {
            pkg(n)
                .deps
                .iter()
                .map(|d| format!("{}@{}", d.name, d.line))
                .collect::<Vec<_>>()
        };
        assert_eq!(deps("shop-web"), ["@angular/core@4", "rxjs@5"]);
        assert_eq!(pkg("shop-web").lines, (2, 2));
        assert_eq!(deps("Shop.Api"), ["Stripe.net@3", "Shop.Core@4"]);
        assert_eq!(deps("orders"), ["github.com/lib/pq@4", "golang.org/x/sync@6"]);
        assert_eq!(deps("worker"), ["requests@4", "pydantic@5"]);
        assert_eq!(m.loose.len(), 1);
        assert_eq!((m.loose[0].0.as_str(), m.loose[0].1.name.as_str()), ("tools", "click"));
    }
}
