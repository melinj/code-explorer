; Rust facts (DESIGN §4.2): the nodes src/lib.rs reads symbols, imports and refs from, outermost
; first in document order (cv_lang_sitter::query::PackQuery). Each pattern's main node is its
; outermost capture; `@callee` is a detail. Which items are symbols (the module-level ones and
; the members of impls, traits and inline modules) and which body each ref belongs to is decided
; in src/lib.rs from the nesting.
[(function_item) (function_signature_item)] @definition.function
(impl_item) @definition.impl
(trait_item) @definition.trait
[(struct_item) (union_item) (enum_item) (type_item) (const_item) (static_item)
 (macro_definition)] @definition.type
(mod_item) @definition.module
(use_declaration) @import
(extern_crate_declaration) @import.crate
[(attribute_item) (inner_attribute_item)] @attribute
(call_expression function: _ @callee) @call
(macro_invocation) @macro
