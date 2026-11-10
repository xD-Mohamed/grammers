// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::collections::{HashMap, HashSet};

use grammers_tl_parser::tl::{Category, Definition, Parameter, ParameterType, Type};

/// Definitions boxed although they do not recurse, by full name (`namespace.name`).
///
/// Holding them inline makes `Message` over 2.5 KiB, `Update` over 5 KiB and every
/// value containing those a large copy at each move through the update path.
/// `wallet.card` (new in layer 233) sits in three bot widget updates: inline it made
/// `Update` 1,264 B, and boxed it only allocates when an update carries a card.
/// Rare, large update variants are boxed so ordinary updates stay small without
/// allocating. Their payload structs and generated conversions keep their shape.
const BOXED_FOR_SIZE: [&str; 16] = [
    "message",
    "messageService",
    "ephemeralMessage",
    "wallet.card",
    "updateSentWalletTransaction",
    "updateServiceNotification",
    "updateSentPhoneCode",
    "updateDraftMessage",
    "updateChannelWebPage",
    "updateWebPage",
    "updateChannelParticipant",
    "updateNotifySettings",
    "updateBotPrecheckoutQuery",
    "updatePeerWallpaper",
    "updateTheme",
    "updateChatParticipant",
];

/// Optional fields held as `Option<Box<T>>`, by definition full name and field name.
///
/// For a large field added to a definition that already existed: boxing the field
/// leaves the definition's enum variant, and with it existing callers, unchanged.
/// `wallet.nftItem` gained `gift` in layer 233, 432 B inline, and it reaches `Update`
/// through `updateSentWalletTransaction`.
/// `messageReplyHeader.reply_media` otherwise reserves the full media enum even
/// for an ordinary text reply, including in short updates and message values.
const BOXED_FIELDS_FOR_SIZE: [(&str, &str); 2] = [
    ("wallet.nftItem", "gift"),
    ("messageReplyHeader", "reply_media"),
];

/// Additional metadata required by several parts of the generation.
pub(crate) struct Metadata<'a> {
    boxed_defs: HashSet<u32>,
    boxed_fields: HashSet<(u32, &'a str)>,
    defs_with_type: HashMap<(&'a Vec<String>, &'a String), Vec<&'a Definition>>,
    unused_flags: HashMap<(&'a Vec<String>, &'a String), Vec<&'a Parameter>>,
}

impl<'a> Metadata<'a> {
    pub fn new(definitions: &'a [Definition]) -> Self {
        let mut metadata = Self {
            boxed_defs: HashSet::new(),
            boxed_fields: HashSet::new(),
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
        type_definitions.iter().for_each(|d| {
            let full_name = d.full_name();
            if BOXED_FOR_SIZE.contains(&full_name.as_str()) {
                metadata.boxed_defs.insert(d.id);
            }
            for (_, field) in BOXED_FIELDS_FOR_SIZE
                .iter()
                .filter(|(def, _)| *def == full_name)
            {
                let param = d
                    .params
                    .iter()
                    .find(|p| p.name == *field)
                    .unwrap_or_else(|| panic!("{full_name} has no field {field} to box"));
                // Only optional values: `None` then costs a pointer, and the accessors
                // generated for fields shared by every variant never cover optional ones.
                assert!(
                    matches!(&param.ty, ParameterType::Normal { ty, flag: Some(_) } if ty.name != "true"),
                    "{full_name}.{field} must be an optional value to be boxed"
                );
                metadata.boxed_fields.insert((d.id, param.name.as_str()));
            }
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

    /// Returns `true` if this optional parameter of `def` is held as `Option<Box<T>>`:
    /// it is one of [`BOXED_FIELDS_FOR_SIZE`].
    pub fn is_boxed_field(&self, def: &Definition, param: &Parameter) -> bool {
        self.boxed_fields.contains(&(def.id, param.name.as_str()))
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
