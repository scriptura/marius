#!/bin/bash
export LC_ALL=C.UTF-8
export LANG=C.UTF-8
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
PROJECT_NAME="$(basename "$PROJECT_ROOT")"
DOCS_DIR="$PROJECT_ROOT/docs"
GEN_DIR="$DOCS_DIR/generate"
OUTPUT="$GEN_DIR/tree.md"
TIMESTAMP=$(date +"%Y-%m-%d %H:%M:%S")

mkdir -p "$GEN_DIR"

cat > "$OUTPUT" << EOF
# Structure du Projet

**Généré le:** $TIMESTAMP

## Racine du Workspace

\`\`\`text
EOF

(
    cd "$PROJECT_ROOT"
    {
        find . -maxdepth 1 -type f -print
        find crates \( -name target -o -name build -o -name artifacts \) -prune -o -print 2>/dev/null
        find db \( -name target -o -name build -o -name doc \) -prune -o -print 2>/dev/null
        find scripts -print 2>/dev/null
    } | tree --fromfile --dirsfirst
) | sed "1s|.*|/$PROJECT_NAME|" >> "$OUTPUT"

cat >> "$OUTPUT" << EOF
\`\`\`

## Documentation du projet

\`\`\`text
EOF

if [ -d "$DOCS_DIR" ]; then
    tree "$DOCS_DIR" --dirsfirst | sed "1s|.*|/$PROJECT_NAME/docs|" >> "$OUTPUT"
else
    echo "⚠️ Dossier $DOCS_DIR introuvable." >> "$OUTPUT"
fi

cat >> "$OUTPUT" << EOF
\`\`\`

EOF

echo -e "\033[0;32m✓\033[0m Arborescence générée : docs/generate/tree.md"
