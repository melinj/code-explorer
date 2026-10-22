; Rust highlight tokens (DESIGN §4.2). Capture names map to token classes
; (cv_lang_sitter::query::capture_class). Tokens are painted in walk order (outer node first) and
; the first capture covering a span wins; on one node the earlier pattern wins, so names go from
; specific to general.

; Whole nodes.
[(line_comment) (block_comment)] @comment
[(string_literal) (raw_string_literal) (char_literal)] @string
[(integer_literal) (float_literal)] @number
(boolean_literal) @constant
[(attribute_item) (inner_attribute_item)] @attribute
[(primitive_type) (type_identifier) (lifetime)] @type
[(self) (crate) (super) (mutable_specifier)] @keyword
(metavariable) @variable
(shorthand_field_identifier) @property

; Fields: called ones are methods.
(call_expression function: (field_expression field: (field_identifier) @function))
(field_identifier) @property

; Identifiers.
(function_item name: (identifier) @function)
(function_signature_item name: (identifier) @function)
(macro_definition name: (identifier) @function)
(macro_invocation (identifier) @function)
(generic_function (identifier) @function)
(call_expression function: (identifier) @function)
(call_expression function: (scoped_identifier name: (identifier) @function))
((identifier) @constant (#match? @constant "^[A-Z0-9_][A-Z0-9_]+$"))
((identifier) @type (#match? @type "^[A-Z]"))

; BEGIN anonymous tokens (generated, see cv_lang_sitter::query::anonymous_block)
[
 "_" "as" "async" "await" "block" "break" "const" "continue"
 "default" "dyn" "else" "enum" "expr" "extern" "false" "fn"
 "for" "gen" "ident" "if" "impl" "in" "item" "let"
 "lifetime" "literal" "loop" "match" "meta" "mod" "move" "pat"
 "pat_param" "path" "pub" "raw" "ref" "return" "static" "stmt"
 "struct" "trait" "true" "try" "tt" "ty" "type" "union"
 "unsafe" "use" "vis" "where" "while" "yield"
] @keyword
[
 "!" "!=" "#" "$" "%" "%=" "&" "&&"
 "&=" "*" "*/" "*=" "+" "+=" "-" "-="
 "->" ".." "..." "..=" "/" "/*" "//" "/="
 "::" "<" "<<" "<<=" "<=" "=" "==" "=>"
 ">" ">=" ">>" ">>=" "?" "@" "^" "^="
 "expr_2021" "macro_rules!" "|" "|=" "||"
] @operator
[
 "\"" "'" "(" ")" "," "." ":" ";"
 "[" "]" "{" "}"
] @punctuation
; END anonymous tokens
