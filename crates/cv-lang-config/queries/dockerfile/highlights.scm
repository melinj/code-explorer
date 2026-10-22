; Dockerfile highlight tokens.
(comment) @comment
["FROM" "AS" "RUN" "CMD" "LABEL" "MAINTAINER" "EXPOSE" "ENV" "ADD" "COPY" "ENTRYPOINT" "VOLUME"
 "USER" "WORKDIR" "ARG" "ONBUILD" "STOPSIGNAL" "HEALTHCHECK" "SHELL" "CROSS_BUILD" "NONE"] @keyword
(image_name) @type
(image_alias) @type
[(double_quoted_string) (single_quoted_string) (json_string) (heredoc_block)] @string
(env_pair name: (_) @property)
(arg_instruction name: (_) @property)
(label_pair key: (_) @property)
(param) @property
[(expansion) (variable)] @variable
(expose_port) @number
["[" "]" "," "=" ":" "@"] @punctuation
