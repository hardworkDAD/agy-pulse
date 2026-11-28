#![deny(clippy::unwrap_used, clippy::expect_used)]

pub(crate) mod app;
pub mod collector;
pub mod config;
pub mod dashboard;
pub mod domain;
pub mod ingest;
pub mod ledger;
pub mod ops;
pub mod platform;
pub(crate) mod profile;
pub mod render;
pub mod sync;

/// Primary CLI entrypoint shared by `agy-pulse` and `agypl` binaries.
pub fn run_cli() -> i32 {
    app::run_cli()
}

#[cfg(test)]
mod topology_tests {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    use std::fs;
    use std::path::{Path, PathBuf};

    const DAG_MODULES: &[&str] = &[
        "domain",
        "platform",
        "profile",
        "render",
        "ingest",
        "ledger",
        "sync",
        "config",
        "collector",
        "dashboard",
        "ops",
    ];

    fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs_files(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    fn source_module_name(src_root: &Path, file: &Path) -> String {
        let rel = file.strip_prefix(src_root).unwrap_or(file);
        let mut comps = rel.components();
        let first = comps
            .next()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default();
        if first == "lib.rs" || first == "main.rs" || first == "bin" {
            "lib".to_string()
        } else {
            first.trim_end_matches(".rs").to_string()
        }
    }

    fn strip_cfg_test_section(content: &str) -> &str {
        content
            .find("\n#[cfg(test)]")
            .map(|idx| &content[..idx])
            .unwrap_or(content)
    }

    fn strip_string_literals(line: &str) -> String {
        let mut out = String::with_capacity(line.len());
        let mut chars = line.chars().peekable();
        let mut in_str = false;
        while let Some(c) = chars.next() {
            if in_str {
                if c == '\\' {
                    let _ = chars.next();
                } else if c == '"' {
                    in_str = false;
                }
            } else if c == '"' {
                in_str = true;
            } else {
                out.push(c);
            }
        }
        out
    }

    fn line_has_rust_unsafe(line: &str) -> bool {
        let code = strip_string_literals(line);
        let bytes = code.as_bytes();
        let needle = b"unsafe";
        let mut i = 0;
        while let Some(pos) = code[i..].find("unsafe") {
            let start = i + pos;
            let end = start + needle.len();
            let left_ok = start == 0
                || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
            let right_ok =
                end == bytes.len() || !(bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_');
            if left_ok && right_ok {
                return true;
            }
            i = end;
        }
        false
    }

    fn extract_crate_refs(content: &str) -> Vec<String> {
        let mut refs = Vec::new();
        for line in content.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            let mut rest = line;
            while let Some(pos) = rest.find("crate::") {
                let after = &rest[pos + "crate::".len()..];
                let ident: String = after
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !ident.is_empty() {
                    refs.push(ident);
                }
                rest = after;
            }
        }
        refs
    }

    #[test]
    fn module_dependency_graph_is_strict_dag_and_enforces_layer_boundaries() {
        let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rs_files(&src_root, &mut files);
        files.sort();

        let known_mods: BTreeSet<&str> =
            DAG_MODULES.iter().copied().chain(["app", "lib"]).collect();
        let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

        for file in &files {
            let content = match fs::read_to_string(file) {
                Ok(c) => c,
                Err(err) => panic!("read source file {}: {err}", file.display()),
            };
            let prod = strip_cfg_test_section(&content);
            let src_mod = source_module_name(&src_root, file);

            // 1. Non-platform production code must have zero `unsafe` keywords and zero `libc::`.
            if src_mod != "platform" {
                for line in prod.lines() {
                    let trimmed = line.trim_start();
                    if trimmed.starts_with("//") {
                        continue;
                    }
                    assert!(
                        !line.contains("libc::"),
                        "forbidden `libc::` in non-platform file {}",
                        file.display()
                    );
                    assert!(
                        !line_has_rust_unsafe(line),
                        "forbidden `unsafe` in non-platform file {}",
                        file.display()
                    );
                }
            }

            // 2. Dashboard must go through collector facade, never `crate::collector::protocol`.
            if src_mod == "dashboard" {
                assert!(
                    !prod.contains("crate::collector::protocol"),
                    "dashboard bypassed collector facade in {}",
                    file.display()
                );
            }

            for ident in extract_crate_refs(prod) {
                if !known_mods.contains(ident.as_str()) {
                    // Bare root `crate::<symbol>` reference (e.g., `crate::VERSION`).
                    assert_ne!(
                        src_mod,
                        "collector",
                        "collector has forbidden root `crate::{ident}` reference in {}",
                        file.display()
                    );
                    edges
                        .entry(src_mod.clone())
                        .or_default()
                        .insert("lib".to_string());
                    continue;
                }
                if ident == src_mod {
                    continue;
                }
                edges.entry(src_mod.clone()).or_default().insert(ident);
            }
        }

        // 3. `domain` has zero cross-module `crate::` references.
        assert!(
            edges.get("domain").is_none_or(BTreeSet::is_empty),
            "domain must have zero cross-module dependencies, got {:?}",
            edges.get("domain")
        );

        // 4. `render` depends ONLY on `domain`.
        let render_deps = edges.get("render").cloned().unwrap_or_default();
        assert_eq!(
            render_deps,
            BTreeSet::from(["domain".to_string()]),
            "render must depend only on domain, got {render_deps:?}"
        );

        // 5. `ledger` has zero `sync` references.
        assert!(
            !edges
                .get("ledger")
                .is_some_and(|deps| deps.contains("sync")),
            "ledger must not depend on sync, got {:?}",
            edges.get("ledger")
        );

        // 6. `collector` has zero `lib` root references.
        assert!(
            !edges
                .get("collector")
                .is_some_and(|deps| deps.contains("lib")),
            "collector must not depend on lib root, got {:?}",
            edges.get("collector")
        );

        // 7. `platform` depends at most on `domain` (TerminalCaps re-export).
        if let Some(plat_deps) = edges.get("platform") {
            assert!(
                plat_deps.iter().all(|d| d == "domain"),
                "platform may only depend on domain, got {plat_deps:?}"
            );
        }

        // 8. Topological sort (Kahn's algorithm) across all 11 architectural modules proves strict DAG.
        let dag_set: BTreeSet<&str> = DAG_MODULES.iter().copied().collect();
        let mut indegree: BTreeMap<&str, usize> =
            DAG_MODULES.iter().map(|&m| (m, 0usize)).collect();
        let mut adj: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for &m in DAG_MODULES {
            if let Some(deps) = edges.get(m) {
                for dep in deps {
                    if let Some(&dep_str) = dag_set.get(dep.as_str())
                        && adj.entry(dep_str).or_default().insert(m)
                    {
                        *indegree.entry(m).or_insert(0) += 1;
                    }
                }
            }
        }

        let mut queue: VecDeque<&str> = indegree
            .iter()
            .filter_map(|(&m, &deg)| (deg == 0).then_some(m))
            .collect();
        let mut visited = 0usize;
        while let Some(node) = queue.pop_front() {
            visited += 1;
            if let Some(neighbors) = adj.get(node) {
                for &next in neighbors {
                    if let Some(deg) = indegree.get_mut(next) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push_back(next);
                        }
                    }
                }
            }
        }
        assert_eq!(
            visited,
            DAG_MODULES.len(),
            "cycle detected in deep-module dependency graph: {edges:?}"
        );
    }
}
