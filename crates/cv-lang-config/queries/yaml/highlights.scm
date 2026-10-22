; YAML highlight tokens (classes by capture name, see src/sitter.rs).
(comment) @comment
(block_mapping_pair key: (flow_node) @property)
(flow_pair key: (flow_node) @property)
[(double_quote_scalar) (single_quote_scalar) (block_scalar)] @string
[(integer_scalar) (float_scalar)] @number
[(boolean_scalar) (null_scalar)] @constant
[(anchor) (alias) (tag)] @type
[(yaml_directive) (tag_directive) (reserved_directive)] @keyword
["-" ":" "," "[" "]" "{" "}" "?"] @punctuation
