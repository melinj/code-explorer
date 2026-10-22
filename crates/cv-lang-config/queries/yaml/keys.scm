; YAML key facts (src/yaml.rs): every block-mapping key (`@pair`, its key `@key`, its value
; `@value`) and every plain list item (`@item`, a scalar or flow collection under `- `).
(block_mapping_pair
  key: (flow_node) @key
  value: (_)? @value) @pair
(block_sequence_item (flow_node) @item)
