; TypeScript facts (DESIGN §4.2): the nodes src/lib.rs reads symbols, imports and refs from,
; outermost first in document order (cv_lang_sitter::query::PackQuery). Each pattern's main node
; is its outermost capture; `@callee` is a detail.

; Declarations; src/lib.rs keeps the module-level ones (directly in the program or under its
; `export`; a `(program …)` pattern here would cost more than the parse).
[(function_declaration) (generator_function_declaration) (interface_declaration)
 (type_alias_declaration) (enum_declaration) (internal_module) (module) (lexical_declaration)
 (variable_declaration)] @declaration
[(class_declaration) (abstract_class_declaration) (class)] @definition.class
(class_body [(method_definition) (public_field_definition)] @definition.member)

(call_expression function: _ @callee) @call
(new_expression constructor: _ @callee) @new
(import_statement) @import
(export_statement source: _) @export.from
