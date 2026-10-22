; HCL key facts (src/hcl.rs): blocks (`@block`, their path is type and labels), attributes
; (`@attribute`: `@name`, `@value`) and object elements (`@elem`: `@key`, `@value`).
(block) @block
(attribute (identifier) @name (expression) @value) @attribute
(object_elem key: (expression) @key val: (expression) @value) @elem
