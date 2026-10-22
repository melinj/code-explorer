; C/C++ facts (DESIGN §4.2): the nodes src/lib.rs reads symbols, includes and refs from, outermost
; first in document order (cv_lang_sitter::query::PackQuery). Each pattern's main node is its
; outermost capture. Which declarations are symbols (those at namespace or class scope), what a
; declaration wrapper adds to a symbol's lines, and which parts of a definition are read is decided
; in src/lib.rs from the nesting.

; Catch2 / doctest: `TEST_CASE("name") { … }` parses as a call statement followed by a block.
; (Listed first: the statement and its call cover the same bytes.)
((expression_statement
   (call_expression function: (identifier) @catch.macro)) @catch
 .
 (compound_statement) @catch.body
 (#any-of? @catch.macro "TEST_CASE" "TEST_CASE_METHOD" "SCENARIO" "TEMPLATE_TEST_CASE"))

(preproc_include) @include
(friend_declaration) @friend
[(template_declaration) (declaration) (field_declaration) (type_definition)] @declaration
(namespace_definition) @definition.namespace
[(class_specifier) (struct_specifier) (union_specifier) (enum_specifier)] @definition.class
(function_definition) @definition.function
[(preproc_def) (preproc_function_def)] @define
(call_expression function: _ @callee) @call
(new_expression) @new
