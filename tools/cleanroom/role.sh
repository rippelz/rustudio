#!/usr/bin/env bash
# Give this session its clean-room role and print that role's rules
# (docs/CLEANROOM.md).
#
#   tools/cleanroom/role.sh reader   # study Studio's internals, write specs, never edit code
#   tools/cleanroom/role.sh writer   # implement specs and all other work, never see internals
#
# In Claude Code, the repo's hook records the role for this session when this
# runs, and enforces it from then on. A session that has been the reader can't
# become the writer. Other tools: follow the rules this prints.
set -euo pipefail
cd "$(dirname "$0")/../.."
role="${1:-}"
case "$role" in
  reader|writer) ;;
  *) sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
agent=".claude/agents/cleanroom-$role.md"
echo "This session is now the clean-room ${role^^}. Its rules (from $agent):"
echo
# The agent file's body: everything after the front matter.
awk 'NR==1 && /^---$/ {fm=1; next} fm && /^---$/ {fm=0; next} !fm' "$agent"
