#!/usr/bin/env bash
# Set up the two clean-room roles as folders, so any Claude Code front end
# (the VS Code extension's chat panel, the CLI, the desktop app) starts in the
# right role just by opening the right folder. See docs/CLEANROOM.md.
#
#   tools/cleanroom/setup-workspaces.sh            # set up both
#   tools/cleanroom/setup-workspaces.sh --undo     # remove both settings
#
# - This repo becomes the WRITER: every session opened here runs as
#   cleanroom-writer (Studio internals blocked). Set in
#   .claude/settings.local.json, which isn't committed.
# - A reader workspace is created next to it, <repo>-cleanroom-reader/: every
#   session opened there runs as cleanroom-reader. It links to this repo's
#   docs/specs/ (specs land in the repo), tools/cleanroom/, docs/CLEANROOM.md
#   and .claude/ agents and hooks, but not the source. Keep Studio dumps in its
#   cleanroom-reference/ folder.
#
# Linux and macOS (uses symlinks). On Windows, use WSL or make the links by hand.
set -euo pipefail
repo="$(cd "$(dirname "$0")/../.." && pwd)"
reader="$repo-cleanroom-reader"

set_agent() {  # set_agent FILE NAME|"" [EXTRA_DIR] : set or remove "agent" (and an extra working dir)
    python3 - "$1" "$2" "${3:-}" <<'EOF'
import json, os, sys
path, name, extra = sys.argv[1], sys.argv[2], sys.argv[3]
data = json.load(open(path)) if os.path.exists(path) else {}
if name:
    data["agent"] = name
else:
    data.pop("agent", None)
if extra:  # the reader's docs/specs link points into the repo; let it write there
    dirs = data.setdefault("permissions", {}).setdefault("additionalDirectories", [])
    if extra not in dirs:
        dirs.append(extra)
os.makedirs(os.path.dirname(path), exist_ok=True)
json.dump(data, open(path, "w"), indent=2)
open(path, "a").write("\n")
EOF
}

if [ "${1:-}" = "--undo" ]; then
    set_agent "$repo/.claude/settings.local.json" ""
    echo "writer default removed from $repo/.claude/settings.local.json"
    [ -d "$reader" ] && echo "reader workspace left in place: $reader (delete it yourself if you want; keep anything in cleanroom-reference/ you need)"
    exit 0
fi

# Writer: this repo.
set_agent "$repo/.claude/settings.local.json" "cleanroom-writer"

# Reader: a separate folder of links, plus its own dump folder.
mkdir -p "$reader/.claude" "$reader/docs" "$reader/tools" "$reader/cleanroom-reference"
link() { ln -sfn "$1" "$2"; }
link "$repo/.claude/agents" "$reader/.claude/agents"
link "$repo/.claude/hooks" "$reader/.claude/hooks"
link "$repo/.claude/skills" "$reader/.claude/skills"
link "$repo/.claude/settings.json" "$reader/.claude/settings.json"
link "$repo/docs/specs" "$reader/docs/specs"
link "$repo/docs/CLEANROOM.md" "$reader/docs/CLEANROOM.md"
link "$repo/tools/cleanroom" "$reader/tools/cleanroom"
set_agent "$reader/.claude/settings.local.json" "cleanroom-reader" "$repo/docs/specs"
cat > "$reader/README.md" <<EOF
# rustudio clean-room reader workspace

Every Claude Code session opened in this folder runs as \`cleanroom-reader\`.
Study Studio's internals here and write specs in \`docs/specs/\`, which is
$repo/docs/specs. Keep dumps and notes in \`cleanroom-reference/\`. The source
tree isn't here on purpose. See docs/CLEANROOM.md.
EOF

echo "writer: $repo  (sessions opened here run as cleanroom-writer)"
echo "reader: $reader  (sessions opened here run as cleanroom-reader)"
echo "Open each folder in its own VS Code window (or run claude in it). Undo: $0 --undo"
