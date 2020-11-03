/// Build a scope digraph.
/// Nodes are items with visibility
/// Directional edges connect nodes to the places where they are visible, i.e.:
/// * fn in mod
/// * struct in mod
/// * struct fields
///     * fields do have visibility
///     * they aren't items, but...
///     * struct effectively acts as a node containing field nodes & conditional edges to them
/// * pub fn in mod directly visible in the parent scope
///     * if parent scope is fully visible to another scope, it is recursively traced
/// * special pub types (pub(crate), etc.)
/// * type aliases
/// * `use super::ABC as XYZ;`
///
/// Possible scope violations, in order of precedence:
/// * In scope, but not allowed to be
///     * Not public
///         * if it's owned by the user, suggest that they should pub it
///         * crates are not owned by user, but any source in the local tree is
///     * Name conflict
///         * can't have two structs in scope with the same name
/// * Out of scope
///     * Exists, but not in scope
///         * fix by adding a use
///             * find disconnected nodes with the same name (expensive?)
///             * see if it's possible to create an edge (reachable)
///                 * don't offer this if it isn't. if it's owned by user it's private and you can could pub it.
///     * Not Found
///         * look for similarly named disconnected nodes and offer a "did you mean"
///             * use [strsim](https://docs.rs/strsim/0.10.0/strsim/) for Ident similarity
///             * heuristic guess by type (fn, struct, var, mod, etc.)
///         * fall back all the way to "not found" if nothing is similar
use codespan_reporting::diagnostic::Diagnostic;
use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};
use rhdl::ast::Tok;

use crate::find_file::{File, FileGraph, FileId};

mod r#use;

mod build;
mod conflicts;
mod graph;
mod path;
mod r#pub;
mod type_existence;

pub use graph::{Branch, Leaf, ResolutionGraph, ResolutionIndex, ResolutionNode};

#[derive(Debug)]
pub struct Resolver<'ast> {
    file_graph: &'ast FileGraph,
    pub resolution_graph: ResolutionGraph<'ast>,
    pub errors: Vec<Diagnostic<FileId>>,
    resolved_uses: HashSet<ResolutionIndex>,
}

impl<'ast> From<&'ast FileGraph> for Resolver<'ast> {
    fn from(file_graph: &'ast FileGraph) -> Self {
        Self {
            file_graph,
            resolution_graph: Default::default(),
            errors: vec![],
            resolved_uses: Default::default(),
        }
    }
}

impl<'ast> Resolver<'ast> {
    /// Find all names given a source forest
    /// Externals are paths to standalone source code: a top + lib.rs of each crate
    pub fn build_graph(&mut self) {
        // Stage one: add nodes
        let files: Vec<FileId> = self.file_graph.roots.clone();
        for file_index in files {
            let file = self.file_graph.inner[file_index].clone();
            let resolution_index = self.resolution_graph.add_node(ResolutionNode::Root {
                // TODO: attach a real name
                name: String::default(),
                children: HashMap::default(),
            });
            self.resolution_graph
                .content_files
                .insert(resolution_index, file);
            let mut builder = build::ScopeBuilder {
                errors: &mut self.errors,
                file_graph: &mut self.file_graph,
                resolution_graph: &mut self.resolution_graph,
                file_ancestry: vec![file_index],
                scope_ancestry: vec![resolution_index],
            };
            builder.visit_file(&self.file_graph.inner[file_index].syn);
        }

        // Stage two: apply visibility
        let mut visibility_errors = self
            .resolution_graph
            .node_indices()
            .filter_map(|i| r#pub::apply_visibility(&mut self.resolution_graph, i).err())
            .collect::<Vec<Diagnostic<FileId>>>();
        self.errors.append(&mut visibility_errors);

        // Stage three: trace use nodes
        let use_indices: Vec<ResolutionIndex> = self
            .resolution_graph
            .node_indices()
            .filter(|i| self.resolution_graph.inner[*i].is_use())
            .collect();
        for use_index in use_indices {
            let mut use_resolver = r#use::UseResolver {
                resolved_uses: &mut self.resolved_uses,
                resolution_graph: &mut self.resolution_graph,
                errors: &mut self.errors,
            };
            use_resolver.resolve_use(use_index);
        }
    }

    pub fn check_graph(&mut self) {
        self.errors.append(&mut self.find_invalid_names());
        {
            let mut conflict_checker = conflicts::ConflictChecker {
                resolution_graph: &self.resolution_graph,
                errors: &mut self.errors,
            };
            conflict_checker.visit_all();
        }
        {
            let mut type_existence_checker = type_existence::TypeExistenceChecker {
                resolution_graph: &self.resolution_graph,
                errors: &mut self.errors,
            };
            type_existence_checker.visit_all();
        }
    }

    fn find_invalid_names(&self) -> Vec<Diagnostic<FileId>> {
        let mut errors = vec![];
        for file_id in self.file_graph.iter() {
            for token in self.file_graph.inner[file_id].to_tokens() {
                if let Tok::Ident(ident) = token {
                    // https://github.com/rust-lang/rust/blob/5ef299eb9805b4c86b227b718b39084e8bf24454/src/librustc_span/symbol.rs#L1592
                    if ident == "r#_"
                        || ident == "r#"
                        || ident == "r#super"
                        || ident == "r#self"
                        || ident == "r#Self"
                        || ident == "r#crate"
                    {
                        errors.push(crate::error::invalid_raw_identifier(file_id, &ident));
                    }
                }
            }
        }
        errors
    }
}
