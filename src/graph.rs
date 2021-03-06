use crate::config::Config;
use crate::dep::{DepKind, ResolvedDep};
use crate::error::CliResult;
use crate::project::DeclaredDepsMap;
use std::collections::HashMap;
use std::fmt;
use std::io::{self, Write};

pub type Node = usize;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone)]
pub struct Edge(pub Node, pub Node);

impl Edge {
    pub fn label<W: Write>(
        &self,
        w: &mut W,
        dg: &DepGraph,
        root_deps_map: &DeclaredDepsMap,
    ) -> io::Result<()> {
        use crate::dep::DepKind::{Build, Dev, Optional, Regular, Unknown};

        let parent = dg.get(self.0).unwrap().kind();
        let child_dep = dg.get(self.1).unwrap();

        // Special case: always color edge from root to root dep by its actual root dependency kind.
        // Otherwise, the root dep could also be a dep of a regular dep which will cause the root ->
        // root dep edge to appear regular, which is misleading as it is not regular in Cargo.toml.
        let child = if self.0 == 0 {
            let kinds = root_deps_map.get(&child_dep.name).unwrap();

            if kinds.contains(&Regular) {
                Regular
            } else if kinds.contains(&Build) {
                Build
            } else if kinds.contains(&Dev) {
                Dev
            } else if kinds.contains(&Optional) {
                Optional
            } else {
                Unknown
            }
        } else {
            child_dep.kind()
        };

        match (parent, child) {
            (Regular, Regular) => writeln!(w, ";"),
            (Build, _) | (Regular, Build) => writeln!(w, " [color=purple, style=dashed];"),
            (Dev, _) | (Regular, Dev) => writeln!(w, " [color=blue, style=dashed];"),
            (Optional, _) | (Regular, Optional) => writeln!(w, " [color=red, style=dashed];"),
            _ => writeln!(w, " [color=orange, style=dashed];"),
        }
    }
}

impl fmt::Display for Edge {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let &Edge(il, ir) = self;
        write!(f, "n{} -> n{}", il, ir)
    }
}

#[derive(Debug)]
pub struct DepGraph {
    pub nodes: Vec<ResolvedDep>,
    pub edges: Vec<Edge>,
    pub cfg: Config,
}

impl DepGraph {
    pub fn new(cfg: Config) -> Self {
        DepGraph {
            nodes: vec![],
            edges: vec![],
            cfg,
        }
    }

    /// Sets the kind of each dependency based on how the dependencies are declared in the manifest.
    pub fn set_resolved_kind(&mut self, declared_deps_map: &HashMap<String, Vec<DepKind>>) {
        self.nodes[0].is_regular = true;

        // Make sure to process edges from the root node first.
        // Sorts by ID of first node first, then by second node.
        self.edges.sort();

        // FIXME: We repeat the following step several times to ensure that the kinds are propogated
        // to all nodes. The surefire way to handle this would be to do a proper topological sort.
        for _ in 0..10 {
            for ed in self.edges.iter() {
                if ed.0 == 0 {
                    // If this is an edge from the root node,
                    // set the kind based on how the dependency is declared in the manifest file.
                    if let Some(kinds) = declared_deps_map.get(&*self.nodes[ed.1].name) {
                        for kind in kinds {
                            match *kind {
                                DepKind::Regular => self.nodes[ed.1].is_regular = true,
                                DepKind::Build => self.nodes[ed.1].is_build = true,
                                DepKind::Dev => self.nodes[ed.1].is_dev = true,
                                DepKind::Optional => self.nodes[ed.1].is_optional = true,
                                _ => (),
                            }
                        }
                    }
                } else {
                    // If this is an edge from a dependency node, propagate the kind. This is a set
                    // of flags because a dependency can appear several times in the graph, and the
                    // kind of dependency may vary based on the path to that dependency. The flags
                    // start at false, and once they become true, they stay true.
                    // ResolvedDep::kind() will pick a kind based on their priority.

                    if self.nodes[ed.0].is_regular {
                        self.nodes[ed.1].is_regular = true;
                    }
                    if self.nodes[ed.0].is_build {
                        self.nodes[ed.1].is_build = true;
                    }
                    if self.nodes[ed.0].is_dev {
                        self.nodes[ed.1].is_dev = true;
                    }
                    if self.nodes[ed.0].is_optional {
                        self.nodes[ed.1].is_optional = true;
                    }
                }
            }
        }
    }

    /// Forces the version to be displayed on dependencies that have the same name (but a different
    /// version) as another dependency.
    pub fn show_version_on_duplicates(&mut self) {
        // Build a list of node IDs, sorted by the name of the dependency on that node.
        let dep_ids_sorted_by_name = {
            let mut deps = self.nodes.iter().enumerate().collect::<Vec<_>>();
            deps.sort_by_key(|dep| &*dep.1.name);
            deps.iter().map(|dep| dep.0).collect::<Vec<_>>()
        };

        for (i, &dep_id_i) in dep_ids_sorted_by_name
            .iter()
            .enumerate()
            .take(dep_ids_sorted_by_name.len() - 1)
        {
            // Find other nodes with the same name.
            // We need to iterate one more time after the last node to handle the break.
            for (j, &dep) in dep_ids_sorted_by_name
                .iter()
                .enumerate()
                .take(dep_ids_sorted_by_name.len() + 1)
                .skip(i + 1)
            {
                // Stop once we've found a node with a different name or reached the end of the
                // list.
                if j >= dep_ids_sorted_by_name.len()
                    || self.nodes[dep_id_i].name != self.nodes[dep].name
                {
                    // If there are at least two nodes with the same name
                    if j >= i + 2 {
                        // Set force_write_ver = true on all nodes
                        // from dep_ids_sorted_by_name[i] to dep_ids_sorted_by_name[j - 1].
                        // Remember: j is pointing on the next node with a *different* name!
                        // Remember also: i..j includes i but excludes j.
                        for &dep_id_k in dep_ids_sorted_by_name.iter().take(j).skip(i) {
                            self.nodes[dep_id_k].force_write_ver = true;
                        }
                    }

                    break;
                }
            }
        }
    }

    pub fn add_child(&mut self, parent: usize, dep_name: &str, dep_ver: &str) -> usize {
        let idr = self.find_or_add(dep_name, dep_ver);
        self.edges.push(Edge(parent, idr));
        idr
    }

    pub fn get(&self, id: usize) -> Option<&ResolvedDep> {
        if id < self.nodes.len() {
            return Some(&self.nodes[id]);
        }
        None
    }

    pub fn remove_orphans(&mut self) {
        let len = self.nodes.len();
        self.edges.retain(|&Edge(idl, idr)| idl < len && idr < len);
        loop {
            let mut removed = false;
            let mut used = vec![false; self.nodes.len()];
            used[0] = true;
            for &Edge(_, idr) in &self.edges {
                used[idr] = true;
            }

            for (id, &u) in used.iter().enumerate() {
                if !u {
                    self.nodes.remove(id);

                    // Remove edges originating from the removed node
                    self.edges.retain(|&Edge(origin, _)| origin != id);
                    // Adjust edges to match the new node indexes
                    for edge in self.edges.iter_mut() {
                        if edge.0 > id {
                            edge.0 -= 1;
                        }
                        if edge.1 > id {
                            edge.1 -= 1;
                        }
                    }
                    removed = true;
                    break;
                }
            }
            if !removed {
                break;
            }
        }
    }

    fn remove_self_pointing(&mut self) {
        loop {
            let mut found = false;
            let mut self_p = vec![false; self.edges.len()];
            for (eid, &Edge(idl, idr)) in self.edges.iter().enumerate() {
                if idl == idr {
                    found = true;
                    self_p[eid] = true;
                    break;
                }
            }

            for (id, &u) in self_p.iter().enumerate() {
                if u {
                    self.edges.remove(id);
                    break;
                }
            }
            if !found {
                break;
            }
        }
    }

    pub fn set_root(&mut self, name: &str, ver: &str) -> bool {
        let root_id = if let Some(i) = self.find(name, ver) {
            i
        } else {
            return false;
        };

        if root_id == 0 {
            return true;
        }

        // Swap with 0
        self.nodes.swap(0, root_id);

        // Adjust edges
        for edge in self.edges.iter_mut() {
            if edge.0 == 0 {
                edge.0 = root_id;
            } else if edge.0 == root_id {
                edge.0 = 0;
            }
            if edge.1 == 0 {
                edge.1 = root_id;
            } else if edge.1 == root_id {
                edge.1 = 0;
            }
        }
        true
    }

    pub fn find(&self, name: &str, ver: &str) -> Option<usize> {
        for (i, d) in self.nodes.iter().enumerate() {
            if d.name == name && d.ver == ver {
                return Some(i);
            }
        }
        None
    }

    pub fn find_or_add(&mut self, name: &str, ver: &str) -> usize {
        if let Some(i) = self.find(name, ver) {
            return i;
        }
        self.nodes
            .push(ResolvedDep::new(name.to_owned(), ver.to_owned()));
        self.nodes.len() - 1
    }

    pub fn render_to<W: Write>(
        mut self,
        output: &mut W,
        root_deps_map: &DeclaredDepsMap,
    ) -> CliResult<()> {
        self.edges.sort();
        self.edges.dedup();
        if !self.cfg.include_orphans {
            self.remove_orphans();
        }
        self.remove_self_pointing();

        writeln!(output, "digraph dependencies {{")?;
        for (i, dep) in self.nodes.iter().enumerate() {
            if let Some(sub_deps) = &self.cfg.subgraph {
                if sub_deps.contains(&dep.name) {
                    // Skip this node, it will be declared in the subgraph.
                    continue;
                }
            }

            write!(output, "\tn{}", i)?;
            dep.label(output, &self.cfg, i)?;
        }
        writeln!(output)?;

        if let Some(sub_deps) = &self.cfg.subgraph {
            writeln!(output, "\tsubgraph cluster_subgraph {{")?;
            if let Some(sub_name) = &self.cfg.subgraph_name {
                writeln!(output, "\t\tlabel=\"{}\";", sub_name)?;
            }
            writeln!(output, "\t\tcolor=brown;")?;
            writeln!(output, "\t\tstyle=dashed;")?;
            writeln!(output)?;

            for (i, dep) in self.nodes.iter().enumerate() {
                if sub_deps.contains(&dep.name) {
                    write!(output, "\t\tn{}", i)?;
                    dep.label(output, &self.cfg, i)?;
                }
            }

            writeln!(output, "\t}}\n")?;
        }

        for ed in &self.edges {
            write!(output, "\t{}", ed)?;
            ed.label(output, &self, root_deps_map)?;
        }
        writeln!(output, "}}")?;

        Ok(())
    }
}
