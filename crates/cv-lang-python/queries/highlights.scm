; Python highlight tokens (DESIGN §4.2). Capture names map to token classes
; (cv_lang_sitter::query::capture_class). The first capture covering a span wins, so whole nodes
; come first (outer before inner: a `type` holds strings and names), then names by precedence,
; then the anonymous tokens.

; Whole nodes.
(type) @type
(comment) @comment
[(string) (concatenated_string)] @string
[(integer) (float)] @number
[(true) (false) (none) (ellipsis)] @constant
(decorator) @decorator

; Names.
(function_definition name: (identifier) @function)
(class_definition name: (identifier) @type)
(call function: (identifier) @function)
(call function: (attribute attribute: (identifier) @function))
(attribute attribute: (identifier) @property)
((identifier) @keyword (#any-of? @keyword "self" "cls"))
((identifier) @constant (#match? @constant "^[A-Z0-9_][A-Z0-9_]+$"))
((identifier) @type (#match? @type "^[A-Z]"))

; BEGIN anonymous tokens (generated, see cv_lang_sitter::query::anonymous_block)
[
 "_" "__future__" "and" "as" "assert" "async" "await" "break"
 "case" "class" "continue" "def" "del" "elif" "else" "except"
 "exec" "finally" "for" "from" "global" "if" "import" "in"
 "is" "lambda" "match" "nonlocal" "not" "or" "pass" "print"
 "raise" "return" "try" "type" "while" "with" "yield"
] @keyword
[
 "!=" "%" "%=" "&" "&=" "*" "**" "**="
 "*=" "+" "+=" "-" "-=" "->" "/" "//"
 "//=" "/=" ":=" "<" "<<" "<<=" "<=" "<>"
 "=" "==" ">" ">=" ">>" ">>=" "@" "@="
 "\\" "^" "^=" "|" "|=" "~"
] @operator
[
 "(" ")" "," "." ":" ";" "[" "]"
 "{" "}"
] @punctuation
; END anonymous tokens
