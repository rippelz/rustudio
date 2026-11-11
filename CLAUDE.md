# CLAUDE.md

@AGENTS.md

Claude Code specifics: the repo's hooks (`.claude/settings.json`) tell each
session its clean-room role when it starts and enforce it on every tool call;
`tools/cleanroom/role.sh reader|writer` claims a role for this session. A
session can also start with a fixed role: `claude --agent cleanroom-reader`
or `claude --agent cleanroom-writer`. `/cleanroom <topic>` runs a whole topic
(reader, spec lint, writer) hands-off. See docs/CLEANROOM.md.
