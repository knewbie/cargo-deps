use crate::config::Config;
use crate::dep::{DeclaredDep, DepKind};
use crate::error::{CliError, CliResult};
use crate::graph::DepGraph;
use crate::util;
use std::collections::HashMap;
use std::path::PathBuf;
use toml::Value;

pub type DeclaredDepsMap = HashMap<String, Vec<DepKind>>;

#[derive(Debug)]
pub struct Project {
    cfg: Config,
}

impl Project {
    pub fn with_config(cfg: Config) -> CliResult<Self> {
        Ok(Project { cfg })
    }

    pub fn graph(
        self,
        manifest_path: PathBuf,
        lock_path: PathBuf,
    ) -> CliResult<(DepGraph, DeclaredDepsMap)> {
        let (root_deps, root_name, root_version) = self.parse_root_deps(&manifest_path)?;

        let mut dg = self.parse_lock_file(lock_path, &root_deps, &root_name, &root_version)?;

        // Set node 0 to be the root.
        if !dg.set_root(&root_name, &root_version) {
            return Err(CliError::Toml("Missing name or version".into()));
        }

        let mut root_deps_map = HashMap::new();
        for dep in root_deps.iter() {
            let (name, kind) = (dep.name.clone(), dep.kind);
            let kinds: &mut Vec<DepKind> = root_deps_map.entry(name).or_insert_with(|| vec![]);
            kinds.push(kind);
        }

        // Set the kind of dependency on each dep.
        dg.set_resolved_kind(&root_deps_map);

        if !self.cfg.include_vers {
            dg.show_version_on_duplicates();
        }

        Ok((dg, root_deps_map))
    }

    /// Builds a list of the dependencies declared in the manifest file.
    pub fn parse_root_deps(
        &self,
        manifest_path: &PathBuf,
    ) -> CliResult<(Vec<DeclaredDep>, String, String)> {
        let manifest_toml = util::toml_from_file(manifest_path)?;

        let mut declared_deps = vec![];

        // Get the name and version of the root project.
        let (root_name, root_version) = {
            if let Some(table) = manifest_toml.get("package") {
                if let Some(table) = table.as_table() {
                    if let (Some(&Value::String(ref n)), Some(&Value::String(ref v))) =
                        (table.get("name"), table.get("version"))
                    {
                        (n.to_string(), v.to_string())
                    } else {
                        return Err(CliError::Toml("No name for 'package'".into()));
                    }
                } else {
                    return Err(CliError::Toml(
                        "Could not parse 'package' as a table".into(),
                    ));
                }
            } else {
                return Err(CliError::Toml("No 'package' table found".into()));
            }
        };

        if let Some(table) = manifest_toml.get("dependencies") {
            if let Some(table) = table.as_table() {
                for (name, dep_table) in table.iter() {
                    if let Some(&Value::Boolean(true)) = dep_table.get("optional") {
                        if self.cfg.optional_deps {
                            declared_deps
                                .push(DeclaredDep::with_kind(name.clone(), DepKind::Optional));
                        }
                    } else if self.cfg.regular_deps {
                        declared_deps.push(DeclaredDep::with_kind(name.clone(), DepKind::Regular));
                    }
                }
            }
        }

        if self.cfg.build_deps {
            if let Some(table) = manifest_toml.get("build-dependencies") {
                if let Some(table) = table.as_table() {
                    for (name, _) in table.iter() {
                        declared_deps.push(DeclaredDep::with_kind(name.clone(), DepKind::Build));
                    }
                }
            }
        }

        if self.cfg.dev_deps {
            if let Some(table) = manifest_toml.get("dev-dependencies") {
                if let Some(table) = table.as_table() {
                    for (name, _) in table.iter() {
                        declared_deps.push(DeclaredDep::with_kind(name.clone(), DepKind::Dev));
                    }
                }
            }
        }

        Ok((declared_deps, root_name, root_version))
    }

    /// Builds a graph of the resolved dependencies declared in the lock file.
    fn parse_lock_file(
        &self,
        lock_path: PathBuf,
        root_deps: &[DeclaredDep],
        name: &str,
        ver: &str,
    ) -> CliResult<DepGraph> {
        let lock_toml = util::toml_from_file(lock_path)?;

        let mut dg = DepGraph::new(self.cfg.clone());

        if let Some(root) = lock_toml.get("root") {
            parse_package(&mut dg, root, root_deps, name, ver);
        }

        if let Some(&Value::Array(ref packages)) = lock_toml.get("package") {
            for pkg in packages {
                parse_package(&mut dg, pkg, root_deps, name, ver);
            }
        }

        Ok(dg)
    }
}

fn parse_package(
    dg: &mut DepGraph,
    pkg: &Value,
    root_deps: &[DeclaredDep],
    root_name: &str,
    root_version: &str,
) {
    let name = pkg
        .get("name")
        .expect("no 'name' field in Cargo.lock [package] or [root] table")
        .as_str()
        .expect(
            "'name' field of [package] or [root] table in Cargo.lock was not a \
             valid string",
        )
        .to_owned();
    let ver = pkg
        .get("version")
        .expect("no 'version' field in Cargo.lock [package] or [root] table")
        .as_str()
        .expect(
            "'version' field of [package] or [root] table in Cargo.lock was not a \
             valid string",
        )
        .to_owned();

    // If --filter was specified, keep only packages that were indicated.
    let filter = dg.cfg.filter.clone();
    if let Some(ref filter_deps) = filter {
        if name != root_name && !filter_deps.contains(&name) {
            return;
        }
    }

    let id = dg.find_or_add(&*name, &*ver);

    if let Some(&Value::Array(ref deps)) = pkg.get("dependencies") {
        for dep in deps {
            let dep_vec = dep.as_str().unwrap_or("").split(' ').collect::<Vec<_>>();
            let dep_name = dep_vec[0].to_owned();
            let dep_ver = dep_vec[1];

            if let Some(ref filter_deps) = filter {
                if !filter_deps.contains(&dep_name) {
                    continue;
                }
            }

            if name == root_name
                && ver == root_version
                && !root_deps.iter().any(|dep| dep.name == dep_name)
            {
                // This dep was filtered out when adding root dependencies.
                continue;
            }

            dg.add_child(id, &*dep_name, dep_ver);
        }
    }
}
