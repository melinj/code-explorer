; TOML highlight tokens: table headers as types, keys as properties.
(table ["[" "]"] @type)
(table [(bare_key) (dotted_key) (quoted_key)] @type)
(table_array_element ["[[" "]]"] @type)
(table_array_element [(bare_key) (dotted_key) (quoted_key)] @type)
(comment) @comment
(pair [(bare_key) (dotted_key) (quoted_key)] @property)
(string) @string
[(integer) (float) (local_date) (local_time) (local_date_time) (offset_date_time)] @number
(boolean) @constant
"=" @operator
["," "." "{" "}" "[" "]"] @punctuation
