; TOML key facts (src/toml.rs): table headers (`@table`, `@array_table` with their `@name`) and
; top-level pairs (`@pair`); the reader descends into pairs, inline tables and arrays itself.
(document (pair) @pair)
(table [(bare_key) (dotted_key) (quoted_key)] @name) @table
(table_array_element [(bare_key) (dotted_key) (quoted_key)] @name) @array_table
