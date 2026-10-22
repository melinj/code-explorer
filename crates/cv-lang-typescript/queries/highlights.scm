; TypeScript / TSX / JavaScript highlight tokens (DESIGN §4.2). Capture names map to token classes
; (cv_lang_sitter::query::capture_class). Tokens are painted in walk order (outer node first) and
; the first capture covering a span wins; on one node the earlier pattern wins, so names go from
; specific to general. TSX inserts queries/jsx.scm at the `@jsx` line below.

; Whole nodes.
[(comment) (html_comment)] @comment
[(string) (template_string) (regex)] @string
(number) @number
[(true) (false) (null) (undefined)] @constant
[(this) (super)] @keyword
(decorator) @decorator
[(type_identifier) (predefined_type)] @type

; Properties: called members and method names are functions.
(call_expression
  function: (member_expression
    property: [(property_identifier) (private_property_identifier)] @function))
(method_definition [(property_identifier) (private_property_identifier)] @function)
[(property_identifier) (private_property_identifier)] @property
[(shorthand_property_identifier) (shorthand_property_identifier_pattern)] @property

; Identifiers.
; @jsx
(function_declaration name: (identifier) @function)
(generator_function_declaration name: (identifier) @function)
(call_expression function: (identifier) @function)
(variable_declarator
  name: (identifier) @function
  value: [(arrow_function) (function_expression) (generator_function)])
((identifier) @constant (#match? @constant "^[A-Z0-9_][A-Z0-9_]+$"))
((identifier) @type (#match? @type "^[A-Z]"))


; BEGIN anonymous tokens (generated, see cv_lang_sitter::query::anonymous_block)
[
 "abstract" "accessor" "any" "as" "assert" "asserts" "async" "await"
 "boolean" "break" "case" "catch" "class" "const" "continue" "debugger"
 "declare" "default" "delete" "do" "else" "enum" "export" "extends"
 "finally" "for" "from" "function" "get" "global" "if" "implements"
 "import" "in" "infer" "instanceof" "interface" "is" "keyof" "let"
 "meta" "module" "namespace" "never" "new" "number" "object" "of"
 "override" "private" "protected" "public" "readonly" "require" "return" "satisfies"
 "set" "static" "string" "switch" "symbol" "target" "throw" "try"
 "type" "typeof" "unknown" "using" "var" "void" "while" "with"
 "yield"
] @keyword
[
 "!" "!=" "!==" "%" "%=" "&" "&&" "&&="
 "&=" "*" "**" "**=" "*=" "+" "++" "+="
 "+?:" "-" "--" "-=" "-?:" "..." "/" "/="
 "<" "<<" "<<=" "<=" "=" "==" "===" "=>"
 ">" ">=" ">>" ">>=" ">>>" ">>>=" "?" "?."
 "?:" "??" "??=" "@" "^" "^=" "unique symbol" "{|"
 "|" "|=" "||" "||=" "|}" "~"
] @operator
[
 "\"" "${" "'" "(" ")" "," "." ":"
 ";" "[" "]" "`" "{" "}"
] @punctuation
; END anonymous tokens
