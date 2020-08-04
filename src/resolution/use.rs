use std::collections::HashSet;
use std::rc::Rc;

use log::error;
use petgraph::{graph::NodeIndex, Direction};
use syn::{ItemMod, ItemUse, UseName, UseRename, UseTree};

use super::{File, Node, ResolutionError, ScopeGraph};
use crate::error::{
    DisambiguationError, GlobAtEntryError, GlobalPathCannotHaveSpecialIdentError,
    SelfNameNotInGroupError, SpecialIdentNotAtStartOfPathError, TooManySupersError,
    UnresolvedItemError, VisibilityError,
};

#[derive(Debug)]
pub enum UseType<'ast> {
    /// Pull a particular name into scope
    /// Could be ambiguous
    Name {
        name: &'ast UseName,
        indices: Vec<NodeIndex>,
    },
    /// Optionally include all items/mods from the scope
    Glob { scope: NodeIndex },
    /// Pull a particular name into scope, but give it a new name (so as to avoid any conflicts)
    /// Could be ambiguous
    Rename {
        rename: &'ast UseRename,
        indices: Vec<NodeIndex>,
    },
}

struct TracingContext<'a, 'ast> {
    scope_graph: &'a mut ScopeGraph<'ast>,
    errors: &'a mut Vec<ResolutionError>,
    file: Rc<File>,
    dest: NodeIndex,
    previous_idents: Vec<syn::Ident>,
    has_leading_colon: bool,
    reentrancy: &'a mut HashSet<NodeIndex>,
}

pub fn trace_use_entry<'a, 'ast>(
    scope_graph: &'a mut ScopeGraph<'ast>,
    errors: &mut Vec<ResolutionError>,
    dest: NodeIndex,
) {
    let (tree, file, has_leading_colon) = match &scope_graph[dest] {
        Node::Use {
            imports,
            item_use,
            file,
            ..
        } => {
            if !imports.is_empty() {
                return;
            }
            (
                &item_use.tree,
                file.clone(),
                item_use.leading_colon.is_some(),
            )
        }
        _ => return,
    };

    let mut reentrancy = HashSet::default();

    trace_use_entry_reenterable(
        &mut TracingContext {
            scope_graph,
            errors,
            file: file,
            dest,
            previous_idents: vec![],
            has_leading_colon,
            reentrancy: &mut reentrancy,
        },
        tree,
    );
}

fn trace_use_entry_reenterable<'a, 'ast>(ctx: &mut TracingContext<'a, 'ast>, tree: &'ast UseTree) {
    if ctx.reentrancy.contains(&ctx.dest) {
        return;
    }
    ctx.reentrancy.insert(ctx.dest);
    let scope = if ctx.has_leading_colon {
        // just give any old dummy node because it'll have to be ignored in path/name finding
        NodeIndex::new(0)
    } else {
        ctx.scope_graph
            .neighbors_directed(ctx.dest, Direction::Incoming)
            .next()
            .unwrap()
    };
    trace_use(ctx, scope, tree, false);
}
/// Trace usages
/// TODO: support ambiguous multi-uses like:
/// ```
/// mod a {
///     pub mod b {}
///     pub fn b() {}
/// }
/// use a::b;
/// ```
/// TODO: crate root imports need to be explicitly named
fn trace_use<'a, 'ast>(
    ctx: &mut TracingContext<'a, 'ast>,
    scope: NodeIndex,
    tree: &'ast UseTree,
    in_group: bool,
) {
    use syn::UseTree::*;
    // Is this the tracing entry point? (value comparison)
    // `item_use.tree` will always be either equal to or a superset of `tree`
    let is_entry = ctx.previous_idents.is_empty();
    match tree {
        Path(path) => {
            let path_ident = path.ident.to_string();
            let new_scope = match path_ident.as_str() {
                // Special keyword cases
                "self" | "super" | "crate" => {
                    let is_last_super = ctx
                        .previous_idents
                        .last()
                        .map(|ident| ident == "super")
                        .unwrap_or_default();
                    if !is_entry && !(path_ident == "super" && is_last_super) {
                        ctx.errors.push(
                            SpecialIdentNotAtStartOfPathError {
                                file: ctx.file.clone(),
                                path_ident: path.ident.clone(),
                            }
                            .into(),
                        );
                        return;
                    }
                    if ctx.has_leading_colon {
                        ctx.errors.push(
                            GlobalPathCannotHaveSpecialIdentError {
                                file: ctx.file.clone(),
                                path_ident: path.ident.clone(),
                            }
                            .into(),
                        );
                        return;
                    }
                    if path_ident == "self" {
                        scope
                    } else if path_ident == "super" {
                        if let Some(use_grandparent) = ctx
                            .scope_graph
                            .neighbors_directed(scope, Direction::Incoming)
                            .next()
                        {
                            use_grandparent
                        } else {
                            ctx.errors.push(
                                TooManySupersError {
                                    file: ctx.file.clone(),
                                    ident: path.ident.clone(),
                                }
                                .into(),
                            );
                            return;
                        }
                    } else if path_ident == "crate" {
                        let mut root = scope;
                        while let Some(next_parent) = ctx
                            .scope_graph
                            .neighbors_directed(root, Direction::Incoming)
                            .next()
                        {
                            root = next_parent;
                        }
                        root
                    } else {
                        error!("the match that led to this arm should prevent this from ever happening");
                        scope
                    }
                }
                // Default case: enter the matching child scope
                _ => {
                    // TODO: check uses for same ident
                    // i.e. use a::b; use b::c;
                    let same_ident_finder = |child: &NodeIndex| {
                        match &ctx.scope_graph[*child] {
                            Node::Mod { item_mod, .. } => item_mod.ident == path.ident.to_string(),
                            // this will work just fine since n is a string
                            Node::Root { name: Some(n), .. } => path.ident == n,
                            _ => false,
                        }
                    };
                    let child = if is_entry && ctx.has_leading_colon {
                        // we know the scope can be ignored in this case...
                        ctx.scope_graph
                            .externals(Direction::Incoming)
                            .find(same_ident_finder)
                    } else if is_entry {
                        let global_child = ctx
                            .scope_graph
                            .externals(Direction::Incoming)
                            .find(same_ident_finder);
                        let local_child = ctx.scope_graph.neighbors(scope).find(same_ident_finder);

                        if let (Some(_gc), Some(_lc)) = (global_child, local_child) {
                            ctx.errors.push(
                                DisambiguationError {
                                    file: ctx.file.clone(),
                                    ident: path.ident.clone(),
                                }
                                .into(),
                            );
                            return;
                        }
                        global_child.or(local_child)
                    } else {
                        ctx.scope_graph.neighbors(scope).find(same_ident_finder)
                    };
                    if child.is_none() {
                        ctx.errors.push(
                            UnresolvedItemError {
                                file: ctx.file.clone(),
                                previous_idents: ctx.previous_idents.clone(),
                                unresolved_ident: path.ident.clone(),
                                has_leading_colon: ctx.has_leading_colon,
                            }
                            .into(),
                        );
                        return;
                    }
                    child.unwrap()
                }
            };
            if !super::visibility::is_target_visible(ctx.scope_graph, ctx.dest, new_scope).unwrap()
            {
                ctx.errors.push(
                    VisibilityError {
                        name_file: ctx.file.clone(),
                        name_ident: path.ident.clone(),
                    }
                    .into(),
                );
                return;
            }
            ctx.previous_idents.push(path.ident.clone());
            trace_use(ctx, new_scope, &path.tree, false);
            ctx.previous_idents.pop();
        }
        Name(UseName { ident, .. }) | Rename(UseRename { ident, .. }) => {
            let original_name_string = ident.to_string();
            let found_children: Vec<NodeIndex> = if original_name_string == "self" {
                if !in_group {
                    ctx.errors.push(
                        SelfNameNotInGroupError {
                            file: ctx.file.clone(),
                            name_ident: ident.clone(),
                        }
                        .into(),
                    );
                    return;
                }
                vec![scope]
            } else {
                // reentrancy behavior
                if !(is_entry && ctx.has_leading_colon) {
                    for reentrant in ctx
                        .scope_graph
                        .neighbors(scope)
                        .filter(|candidate| *candidate != ctx.dest)
                        .filter(|candidate| !ctx.reentrancy.contains(&candidate))
                        .filter(|candidate: &NodeIndex| match &ctx.scope_graph[*candidate] {
                            Node::Use {
                                imports: other_use_imports,
                                ..
                            } => other_use_imports.is_empty(),
                            _ => false,
                        })
                        .collect::<Vec<NodeIndex>>()
                    {
                        let (other_use_tree, other_use_file, other_use_has_leading_colon) =
                            match &ctx.scope_graph[reentrant] {
                                Node::Use {
                                    item_use:
                                        ItemUse {
                                            tree: other_use_tree,
                                            leading_colon,
                                            ..
                                        },
                                    file: other_use_file,
                                    ..
                                } => (
                                    other_use_tree,
                                    other_use_file.clone(),
                                    leading_colon.is_some(),
                                ),
                                _ => continue,
                            };
                        let mut rebuilt_ctx = TracingContext {
                            scope_graph: ctx.scope_graph,
                            errors: ctx.errors,
                            file: other_use_file,
                            dest: reentrant,
                            previous_idents: vec![],
                            has_leading_colon: other_use_has_leading_colon,
                            reentrancy: ctx.reentrancy,
                        };
                        trace_use_entry_reenterable(&mut rebuilt_ctx, other_use_tree);
                    }
                }
                let child_matcher = |child: &NodeIndex| match &ctx.scope_graph[*child] {
                    Node::Var { ident, .. }
                    | Node::Macro { ident, .. }
                    | Node::Type { ident, .. } => **ident == original_name_string,
                    Node::Fn { item_fn, .. } => item_fn.sig.ident == original_name_string,
                    Node::Mod {
                        item_mod: ItemMod { ident, .. },
                        ..
                    } => *ident == original_name_string,
                    Node::Root { name: Some(n), .. } => original_name_string == *n,
                    Node::Root { name: None, .. } => false,
                    Node::Impl { .. } => false,
                    Node::Use {
                        imports: other_use_imports,
                        ..
                    } => {
                        if other_use_imports.is_empty() {
                            error!("uses that aren't traced yet can't be resolved");
                            return false;
                        }
                        other_use_imports.iter().any(|(_, use_types)| {
                            use_types.iter().any(|use_type| match use_type {
                                UseType::Name { name, .. } => name.ident == original_name_string,
                                UseType::Rename { rename, .. } => {
                                    rename.rename == original_name_string
                                }
                                _ => false,
                            })
                        })
                    }
                };

                if is_entry && ctx.has_leading_colon {
                    // special resolution required
                    ctx.scope_graph
                        .externals(Direction::Incoming)
                        .find(child_matcher)
                        .map(|child| vec![child])
                        .unwrap_or_default()
                } else if is_entry {
                    let global_child = ctx
                        .scope_graph
                        .externals(Direction::Incoming)
                        .find(child_matcher);
                    let local_children = ctx
                        .scope_graph
                        .neighbors(scope)
                        .filter(|child| *child != ctx.dest)
                        .filter(child_matcher)
                        .collect::<Vec<NodeIndex>>();
                    if let (Some(_gc), true) = (global_child, !local_children.is_empty()) {
                        ctx.errors.push(
                            DisambiguationError {
                                file: ctx.file.clone(),
                                ident: ident.clone(),
                            }
                            .into(),
                        );
                        return;
                    }
                    global_child.map(|gc| vec![gc]).unwrap_or(local_children)
                } else {
                    // todo: unwrap_or_else look for first glob implicit import
                    ctx.scope_graph
                        .neighbors(scope)
                        .filter(|child| *child != ctx.dest)
                        .filter(child_matcher)
                        .collect()
                }
            };
            if found_children.is_empty() {
                ctx.errors.push(
                    UnresolvedItemError {
                        file: ctx.file.clone(),
                        previous_idents: ctx.previous_idents.clone(),
                        unresolved_ident: ident.clone(),
                        has_leading_colon: ctx.has_leading_colon,
                    }
                    .into(),
                );
                return;
            };
            let found_children = found_children
                .iter()
                .filter(|index| {
                    super::visibility::is_target_visible(ctx.scope_graph, ctx.dest, **index)
                        .unwrap()
                })
                .cloned()
                .collect::<Vec<NodeIndex>>();
            if found_children.is_empty() {
                ctx.errors.push(
                    VisibilityError {
                        name_file: ctx.file.clone(),
                        name_ident: ident.clone(),
                    }
                    .into(),
                );
                return;
            }
            if let Node::Use { imports, .. } = &mut ctx.scope_graph[ctx.dest] {
                match tree {
                    Name(name) => imports.entry(scope).or_default().push(UseType::Name {
                        name,
                        indices: found_children,
                    }),
                    Rename(rename) => imports.entry(scope).or_default().push(UseType::Rename {
                        rename,
                        indices: found_children,
                    }),
                    _ => {}
                }
            }
        }
        Glob(glob) => {
            if is_entry
                || ctx.has_leading_colon
                || ctx
                    .previous_idents
                    .last()
                    .map(|ident| ident == "self")
                    .unwrap_or_default()
            {
                ctx.errors.push(
                    GlobAtEntryError {
                        file: ctx.file.clone(),
                        star_span: glob.star_token.spans[0].clone(),
                        has_leading_colon: ctx.has_leading_colon,
                    }
                    .into(),
                );
                return;
            }
            if let Node::Use { imports, .. } = &mut ctx.scope_graph[ctx.dest] {
                imports
                    .entry(scope)
                    .or_default()
                    .push(UseType::Glob { scope })
            }
        }
        Group(group) => group
            .items
            .iter()
            .for_each(|tree| trace_use(ctx, scope, tree, true)),
    }
}
