//! The table of `sym` symbols (including all modifier variants).

use std::sync::OnceLock;

use codex::{Def, Module};

use crate::SymbolInfo;

fn collect(module: Module, prefix: &str, out: &mut Vec<SymbolInfo>) {
    for (name, binding) in module.iter() {
        let full = if prefix.is_empty() { name.to_string() } else { format!("{prefix}.{name}") };
        match binding.def {
            Def::Module(inner) => collect(inner, &full, out),
            Def::Symbol(sym) => {
                let first = out.len();
                let mut has_bare = false;
                for (modifiers, value, deprecation) in sym.variants() {
                    let mods = modifiers.as_str();
                    has_bare |= mods.is_empty();
                    let name = if mods.is_empty() { full.clone() } else { format!("{full}.{mods}") };
                    let mut chars = value.chars();
                    let ch = match (chars.next(), chars.next()) {
                        (Some(c), None) => Some(c),
                        _ => None,
                    };
                    out.push(SymbolInfo { name, value: value.to_string(), ch, deprecated: binding.deprecation.is_some() || deprecation.is_some() });
                }
                // The bare name resolves to the first variant.
                if !has_bare && let Some(default) = out.get(first).cloned() {
                    out.insert(first, SymbolInfo { name: full.clone(), ..default });
                }
            }
        }
    }
}

pub fn symbols() -> &'static [SymbolInfo] {
    static TABLE: OnceLock<Vec<SymbolInfo>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut out = Vec::new();
        collect(codex::SYM, "", &mut out);
        out
    })
}
