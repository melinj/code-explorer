; Shell highlight tokens.
(comment) @comment
(function_definition name: (word) @function)
(command_name (word) @function)
[(string) (raw_string) (ansi_c_string) (translated_string) (heredoc_body)] @string
(number) @number
[(simple_expansion) (expansion) (variable_name)] @variable
(file_descriptor) @number
["if" "then" "else" "elif" "fi" "case" "esac" "for" "select" "while" "until" "do" "done" "in"
 "function" "export" "local" "declare" "readonly" "typeset" "unset" "unsetenv"] @keyword
["&&" "||" "|" "|&" "&" ";" ";;" "<" ">" ">>" "&>" "&>>" "<<" "<<-" "<<<" "=" "!"] @operator
["(" ")" "{" "}" "[" "]" "[[" "]]" "$(" "${" "`"] @punctuation
