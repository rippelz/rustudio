#!/usr/bin/env bash
# Start a clean-room session you can talk to (docs/CLEANROOM.md).
#
#   tools/cleanroom/session.sh reader <topic>   # study Studio, write docs/specs/<topic>.md
#   tools/cleanroom/session.sh writer <topic>   # implement docs/specs/<topic>.md
#
# Run each in its own terminal (or terminal tab) and switch between them.
# The same thing by hand: claude --agent cleanroom-reader|cleanroom-writer
set -euo pipefail
cd "$(dirname "$0")/../.."
role="${1:-}"; topic="${2:-}"
case "$role" in
  reader)
    prompt="Topic: ${topic:-<ask me>}. We're writing docs/specs/${topic:-<topic>}.md. Start by checking whether the spec exists and has writer questions, then tell me what you plan to look at."
    ;;
  writer)
    [ -n "$topic" ] || { echo "usage: $0 writer <topic>" >&2; exit 2; }
    [ -f "docs/specs/$topic.md" ] || { echo "no spec yet: docs/specs/$topic.md (run the reader first)" >&2; exit 1; }
    prompt="Implement docs/specs/$topic.md. Start by reading it and telling me your plan and any questions."
    ;;
  *)
    sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2 ;;
esac
exec claude --agent "cleanroom-$role" "$prompt"
