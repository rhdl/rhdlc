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
use rhdl::{
    ast::{ToTokens, Tok},
    visit::Visit,
};

use crate::find_file::{FileGraph, FileId};

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
    ctx: &'ast z3::Context,
    vis_solver: r#pub::VisibilitySolver<'ast>,
    resolved_uses: HashSet<ResolutionIndex>,
}

impl<'ast> Resolver<'ast> {
    pub fn build(file_graph: &'ast FileGraph, ctx: &'ast z3::Context) -> Self {
        // Stage one: add nodes
        let files: Vec<FileId> = file_graph.roots.clone();
        let mut resolution_graph: ResolutionGraph<'ast> = Default::default();
        let mut errors = vec![];
        for file_index in files {
            let resolution_index = resolution_graph.add_node(ResolutionNode::Root {
                // TODO: attach a real name
                name: String::default(),
                children: HashMap::default(),
            });
            resolution_graph
                .content_files
                .insert(resolution_index, file_index);
            let mut builder = build::ScopeBuilder {
                errors: &mut errors,
                file_graph: &file_graph,
                resolution_graph: &mut resolution_graph,
                file_ancestry: vec![file_index],
                scope_ancestry: vec![resolution_index],
            };
            if let Some(parsed) = &file_graph[file_index].parsed {
                builder.visit_file(parsed);
            }
        }

        Self {
            vis_solver: r#pub::build_visibility_solver(&mut resolution_graph, &mut errors, ctx),
            file_graph,
            resolution_graph,
            errors,
            ctx,
            resolved_uses: Default::default(),
        }
    }

    pub fn build_graph(&mut self) {
        // // Stage three: trace use nodes
        let use_indices: Vec<ResolutionIndex> = self
            .resolution_graph
            .node_indices()
            .filter(|i| self.resolution_graph[*i].is_use())
            .collect();
        for use_index in use_indices {
            let mut use_resolver = r#use::UseResolver {
                resolved_uses: &mut self.resolved_uses,
                vis_solver: &self.vis_solver,
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
                vis_solver: &self.vis_solver,
                errors: &mut self.errors,
            };
            type_existence_checker.visit_all();
        }
    }

    fn find_invalid_names(&self) -> Vec<Diagnostic<FileId>> {
        let mut errors = vec![];
        for file_id in self.file_graph.iter().cloned() {
            if let Some(parsed) = &self.file_graph[file_id].parsed {
                for token in parsed.to_tokens() {
                    if let Tok::Ident(ident) = token {
                        let inner = &ident.inner;
                        // https://github.com/rust-lang/rust/blob/5ef299eb9805b4c86b227b718b39084e8bf24454/src/librustc_span/symbol.rs#L1592
                        if inner == "r#_"
                            || inner == "r#"
                            || inner == "r#super"
                            || inner == "r#self"
                            || inner == "r#Self"
                            || inner == "r#crate"
                        {
                            errors.push(crate::error::invalid_raw_identifier(file_id, &ident));
                        }
                    }
                }
            }
        }
        errors
    }
}
