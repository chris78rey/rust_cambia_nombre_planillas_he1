#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="${HOME}/.local/bin"
TARGET="${BIN_DIR}/he1-unificar-pdfs"

mkdir -p "$BIN_DIR"

cat > "$TARGET" <<EOF
#!/usr/bin/env bash
set -euo pipefail
exec cargo run --quiet --manifest-path "$ROOT_DIR/Cargo.toml" -- "\$@"
EOF

chmod +x "$TARGET"

case ":${PATH}:" in
  *":${BIN_DIR}:"*)
    ;;
  *)
    echo "Agrega esto a tu shell si aun no esta en PATH:"
    echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
    ;;
esac

echo "Instalado: $TARGET"
