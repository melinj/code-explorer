; TSX only (spliced into highlights.scm at `; @jsx`): element names are tags.
(jsx_opening_element (identifier) @tag)
(jsx_closing_element (identifier) @tag)
(jsx_self_closing_element (identifier) @tag)

; JSX closing tokens (anonymous tokens only TSX has).
["</" "/>"] @operator
