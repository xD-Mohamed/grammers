// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::collections::{HashMap, HashSet};

use grammers_tl_parser::tl::{Category, Definition, Parameter, ParameterType, Type};

/// Definitions boxed although they do not recurse.
///
/// Holding them inline makes `Message` over 2.5 KiB, `Update` over 5 KiB and every
/// value containing those a large copy at each move through the update path.
const BOXED_FOR_SIZE: [&str; 3] = ["message", "messageService", "ephemeralMessage"];

/// Additional metadata required by several parts of the generation.
pub(crate) struct Metadata<'a> {
    boxed_defs: HashSet<u32>,
    defs_with_type: HashMap<(&'a Vec<String>, &'a String), Vec<&'a Definition>>,
    unused_flags: HashMap<(&'a Vec<String>, &'a String), Vec<&'a Parameter>>,
}

impl<'a> Metadata<'a> {
    pub fn new(definitions: &'a [Definition]) -> Self {
        let mut metadata = Self {
            boxed_defs: HashSet::new(),
            defs_with_type: HashMap::new(),
            unused_flags: HashMap::new(),
        };

        definitions.iter().for_each(|d| {
            d.params
                .iter()
                .filter(|pf| matches!(pf.ty, ParameterType::Flags))
                .for_each(|pf| {
                    if !d.params.iter().any(|pn| match &pn.ty {
                        ParameterType::Normal {
                            flag: Some(flag), ..
                        } => flag.name == pf.name,
                        _ => false,
                    }) {
                        metadata
                            .unused_flags
                            .entry((&d.namespace, &d.name))
                            .or_default()
                            .push(pf)
                    }
                })
        });

        let type_definitions = definitions
            .iter()
            .filter(|d| d.category == Category::Types)
            .collect::<Vec<_>>();

        type_definitions.iter().for_each(|d| {
            metadata
                .defs_with_type
                .entry((&d.ty.namespace, &d.ty.name))
                .or_default()
                .push(d);
        });

        type_definitions.iter().for_each(|d| {
            if def_self_references(d, d, &metadata.defs_with_type, &mut HashSet::new()) {
                metadata.boxed_defs.insert(d.id);
            }
        });
        type_definitions
            .iter()
            .filter(|d| d.namespace.is_empty() && BOXED_FOR_SIZE.contains(&d.name.as_str()))
            .for_each(|d| {
                metadata.boxed_defs.insert(d.id);
            });

        metadata
    }

    pub fn is_unused_flag(&self, def: &Definition, flag: &Parameter) -> bool {
        self.unused_flags
            .get(&(&def.namespace, &def.name))
            .map(|flags| flags.contains(&flag))
            .unwrap_or(false)
    }

    /// Returns `true` if the enum variant of this `Definition` holds it in a `Box`:
    /// any of its parameters eventually contains the same type as the `Definition`
    /// itself (meaning it recurses), or it is one of [`BOXED_FOR_SIZE`].
    pub fn is_boxed_def(&self, def: &Definition) -> bool {
        self.boxed_defs.contains(&def.id)
    }

    pub fn defs_with_type(&self, ty: &'a Type) -> &Vec<&Definition> {
        &self.defs_with_type[&(&ty.namespace, &ty.name)]
    }
}

fn def_self_references(
    root: &Definition,
    check: &Definition,
    defs_with_type: &HashMap<(&Vec<String>, &String), Vec<&Definition>>,
    visited: &mut HashSet<u32>,
) -> bool {
    visited.insert(check.id);
    for param in check.params.iter() {
        match &param.ty {
            ParameterType::Flags => {}
            ParameterType::Normal { ty, .. } => {
                if ty.namespace == root.ty.namespace && ty.name == root.ty.name {
                    return true;
                }

                if let Some(defs) = defs_with_type.get(&(&ty.namespace, &ty.name)) {
                    for def in defs {
                        if visited.contains(&def.id) {
                            continue;
                        }
                        if def_self_references(root, def, defs_with_type, visited) {
                            return true;
                        }
                    }
                }
            }
        }
    }

    false
}
