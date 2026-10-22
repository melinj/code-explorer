; C/C++ highlight tokens (DESIGN §4.2). Capture names map to token classes
; (cv_lang_sitter::query::capture_class). Tokens are painted in walk order (outer node first) and
; the first capture covering a span wins; on one node the earlier pattern wins. A `@name` capture
; is plain and a `@property.member` a property unless src/lib.rs knows the node as a called or
; declared function (or a `#define`d name) from reading the tags: which names are functions depends
; on declarators, qualified names and the scopes that are read, which a query cannot follow.

; Whole nodes.
(comment) @comment
[(string_literal) (raw_string_literal) (char_literal) (system_lib_string)] @string
(number_literal) @number
[(true) (false) (null)] @constant
(attribute_declaration) @attribute
[(primitive_type) (type_identifier) (sized_type_specifier) (auto) (namespace_identifier)] @type
(this) @keyword
; A destructor or operator name, when it names a function (else its parts are read).
[(destructor_name) (operator_name)] @name.function

; Names.
(field_identifier) @property.member
(identifier) @name
((identifier) @constant (#match? @constant "^[A-Z0-9_][A-Z0-9_]+$"))

; BEGIN anonymous tokens (generated, see cv_lang_sitter::query::anonymous_block)
[
 "#define" "#elif" "#elifdef" "#elifndef" "#else" "#endif" "#if" "#ifdef"
 "#ifndef" "#include" "NULL" "_Alignas" "_Alignof" "_Atomic" "_Generic" "_Nonnull"
 "_Noreturn" "__alignof" "__alignof__" "__asm" "__asm__" "__attribute" "__attribute__" "__based"
 "__cdecl" "__clrcall" "__declspec" "__except" "__extension__" "__fastcall" "__finally" "__forceinline"
 "__inline" "__inline__" "__leave" "__restrict__" "__stdcall" "__thiscall" "__thread" "__try"
 "__unaligned" "__vectorcall" "__volatile__" "_alignof" "_unaligned" "alignas" "alignof" "and"
 "and_eq" "asm" "bitand" "bitor" "break" "case" "catch" "class"
 "co_await" "co_return" "co_yield" "compl" "concept" "const" "consteval" "constexpr"
 "constinit" "continue" "decltype" "default" "defined" "delete" "do" "else"
 "enum" "explicit" "extern" "final" "for" "friend" "goto" "if"
 "inline" "long" "mutable" "namespace" "new" "noexcept" "noreturn" "not"
 "not_eq" "nullptr" "offsetof" "operator" "or" "or_eq" "override" "private"
 "protected" "public" "register" "requires" "restrict" "return" "short" "signed"
 "sizeof" "static" "static_assert" "struct" "switch" "template" "thread_local" "throw"
 "try" "typedef" "typename" "union" "unsigned" "using" "virtual" "volatile"
 "while" "xor" "xor_eq"
] @keyword
[
 "\n" "!" "!=" "\"\"" "%" "%=" "&" "&&"
 "&=" "()" "*" "*=" "+" "++" "+=" "-"
 "--" "-=" "->" "->*" ".*" "..." "/" "/="
 "::" "<" "<<" "<<=" "<=" "<=>" "=" "=="
 ">" ">=" ">>" ">>=" "?" "L\"" "L'" "LR\""
 "R\"" "U\"" "U'" "UR\"" "[[" "[]" "]]" "^"
 "^=" "u\"" "u'" "u8\"" "u8'" "u8R\"" "uR\"" "|"
 "|=" "||" "~"
] @operator
[
 "\"" "'" "(" ")" "," "." ":" ";"
 "[" "]" "{" "}"
] @punctuation
; END anonymous tokens
