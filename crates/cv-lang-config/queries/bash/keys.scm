; Shell key facts (src/shell.rs): `export NAME=value` (`@export`), `NAME=value cmd` environment
; prefixes (`@env`) and every command (`@command`, its logical line becomes an `sh` fact).
(declaration_command "export" (variable_assignment name: (variable_name) @name value: (_)? @value) @export)
(command (variable_assignment name: (variable_name) @name value: (_)? @value) @env)
(command) @command
