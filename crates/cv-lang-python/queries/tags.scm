; Python facts (DESIGN §4.2): the nodes src/lib.rs reads symbols, imports and refs from.
; Matches are read outermost first in document order (cv_lang_sitter::query::PackQuery).
; Each pattern captures its main node; `@callee` is a detail of `@call`.
(class_definition) @definition.class
(function_definition) @definition.function
(call function: [(identifier) (attribute)] @callee) @call
(import_statement) @import
(import_from_statement) @import.from
