; Dockerfile key facts (src/docker.rs): ENV pairs, ARG names (with defaults), and the commands
; of CMD / ENTRYPOINT / RUN (their logical line is the value). Top-level instructions only (not
; the CMD of a HEALTHCHECK or an ONBUILD trigger).
(source_file (env_instruction (env_pair name: (_) @name value: (_)? @value)) @env)
(source_file (arg_instruction name: (_) @name default: (_)? @value) @arg)
(source_file [(cmd_instruction) (entrypoint_instruction) (run_instruction)] @command)
