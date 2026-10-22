; HCL / Terraform highlight tokens.
(comment) @comment
(block . (identifier) @keyword)
(attribute (identifier) @property)
(object_elem key: (expression (variable_expr (identifier) @property)))
[(string_lit) (quoted_template) (heredoc_template)] @string
(numeric_lit) @number
[(bool_lit) (null_lit)] @constant
(function_call (identifier) @function)
["for" "in" "if" "else" "endif" "endfor"] @keyword
["=" "=>" "?" ":" "!" "==" "!=" "<" ">" "<=" ">=" "&&" "||" "+" "-" "*" "/" "%"] @operator
[(block_start) (block_end) (object_start) (object_end) (tuple_start) (tuple_end) "(" ")" "," "."] @punctuation
